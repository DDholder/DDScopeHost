import test from "node:test";
import assert from "node:assert/strict";
import { BridgeClient, HighFreqTopicBus } from "../src/bridge/index.ts";
import {
  hexToRgbaNumber,
  rgbaNumberToCss,
  rgbaNumberToHex,
} from "../src/scope-engine/time-format.ts";
import {
  BOARD_CONFIG_SCHEMA_V1,
  CANVAS_CONFIG_SCHEMA_V1,
  PROJECT_CONFIG_SCHEMA_V1,
  SOURCES_CONFIG_SCHEMA_V1,
  parseBoardConfigJson,
  parseProjectConfigJson,
  parseSourcesConfigJson,
  rehydrateBoardFromConfig,
} from "../src/workspace/config-io.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("rgbaNumberToHex and hexToRgbaNumber preserve unsigned 32-bit RGBA colors", () => {
  const rose = 0xfb7185ff;
  assert.equal(rose > 0x7fffffff, true);
  assert.equal(rgbaNumberToHex(rose), "#fb7185");
  assert.equal(hexToRgbaNumber("#fb7185"), rose);
  assert.equal(hexToRgbaNumber("#FB7185"), 0xfb7185ff);
  assert.equal(rgbaNumberToCss(hexToRgbaNumber("#12abef"), 0.8), "rgba(18, 171, 239, 0.8)");
});

test("Waveform tracks across analog, digital, and state_ribbon lanes support color customization", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    const channels = Object.values(store.getState().channelsById);
    const analogCh = channels.find((c) => c.modality === "scalar");
    const digitalCh = channels.find((c) => c.modality === "digital_bool");
    const fsmCh = channels.find((c) => c.modality === "state_machine");
    assert.ok(analogCh && digitalCh && fsmCh);

    const newScopeId = store.getState().addScope("Color Test Scope");
    store.getState().bindChannelToScope(newScopeId, digitalCh.channel_id);
    store.getState().bindChannelToScope(newScopeId, fsmCh.channel_id);

    const analogColor = hexToRgbaNumber("#ef4444");
    const digitalColor = hexToRgbaNumber("#22d3ee");
    const fsmColor = hexToRgbaNumber("#a855f7");

    store.getState().updateScope(newScopeId, (prev) => ({
      ...prev,
      tracks: prev.tracks.map((tr) => {
        if (tr.lane === "analog") return { ...tr, colorRgba: analogColor };
        if (tr.lane === "digital") return { ...tr, colorRgba: digitalColor };
        return { ...tr, colorRgba: fsmColor };
      }),
    }));

    const updatedScope = store.getState().scopes[newScopeId];
    assert.equal(updatedScope.tracks.find((tr) => tr.lane === "analog")?.colorRgba, analogColor);
    assert.equal(updatedScope.tracks.find((tr) => tr.lane === "digital")?.colorRgba, digitalColor);
    assert.equal(updatedScope.tracks.find((tr) => tr.lane === "state_ribbon")?.colorRgba, fsmColor);
  } finally {
    client.stopDataPlane();
  }
});

test("Sources config export/import round-trips all or single source and auto re-binds existing tracks/widgets", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    await store.getState().createSource({
      source_id: "udp_rig1",
      display_name: "Bench Rig UDP",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9100",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: null,
      },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 50_000,
      },
      protocol: {
        codec: "just_float",
        channel_names: ["v_phase", "i_phase"],
      },
      auto_start: false,
    });

    // Customize scope track conditioning & color on sim0 before exporting
    const simChannels = Object.values(store.getState().channelsById).filter(
      (c) => c.source_id === "sim0" && c.modality === "scalar",
    );
    const simTrackCh = simChannels[0];
    assert.ok(simTrackCh);

    const customColor = hexToRgbaNumber("#84cc16");
    store.getState().updateScope(1, (prev) => ({
      ...prev,
      tracks: prev.tracks.map((tr) =>
        tr.channelId === simTrackCh.channel_id
          ? { ...tr, colorRgba: customColor, yScale: 2.5, yOffset: -3.5 }
          : tr,
      ),
    }));

    // Export all sources + single source check
    const allJson = store.getState().exportSourcesConfig();
    const parsedAll = parseSourcesConfigJson(allJson);
    assert.equal(parsedAll.schema, SOURCES_CONFIG_SCHEMA_V1);
    assert.equal(parsedAll.sources.length, 2);
    const sim0Cfg = parsedAll.sources.find((s) => s.source_id === "sim0");
    assert.ok(sim0Cfg);
    assert.equal(sim0Cfg.transport.kind, "sim");
    if (sim0Cfg.transport.kind === "sim") {
      assert.equal(sim0Cfg.transport.sample_rate_hz, 2_000_000);
    }

    const singleJson = store.getState().exportSourcesConfig(["udp_rig1"]);
    const parsedSingle = parseSourcesConfigJson(singleJson);
    assert.equal(parsedSingle.sources.length, 1);
    assert.equal(parsedSingle.sources[0].source_id, "udp_rig1");

    // Delete udp_rig1, then import allJson with replaceExisting: false first (skips sim0, adds udp_rig1)
    await store.getState().removeSource("udp_rig1");
    assert.equal(store.getState().channelGroups.length, 1);

    const skipRes = await store.getState().importSourcesConfig(allJson, {
      replaceExisting: false,
    });
    assert.deepEqual(skipRes.importedIds, ["udp_rig1"]);
    assert.deepEqual(skipRes.skippedIds, ["sim0"]);

    // Now import with replaceExisting: true so sim0 gets removed + recreated with shifted numeric IDs
    const oldSimChId = simTrackCh.channel_id;
    const replaceRes = await store.getState().importSourcesConfig(allJson, {
      replaceExisting: true,
    });
    assert.deepEqual(replaceRes.importedIds, ["sim0", "udp_rig1"]);
    assert.deepEqual(replaceRes.skippedIds, []);

    const newSimCh = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === simTrackCh.full_path,
    );
    assert.ok(newSimCh);
    assert.notEqual(newSimCh.channel_id, oldSimChId);

    // Track & widget must have re-bound automatically by full_path with preserved color/scale/offset
    const reboundTrack = store
      .getState()
      .scopes[1].tracks.find((tr) => tr.channelId === newSimCh.channel_id);
    assert.ok(reboundTrack);
    assert.equal(reboundTrack.colorRgba, customColor);
    assert.equal(reboundTrack.yScale, 2.5);
    assert.equal(reboundTrack.yOffset, -3.5);
    assert.ok(store.getState().scopes[1].trigger !== null);
  } finally {
    client.stopDataPlane();
  }
});

test("Whole-page board config export/import restores Dockview panes, all scopes, canvases, widget bindings, UI state, and downlink settings", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();

    const channels = Object.values(store.getState().channelsById);
    const analogCh = channels.find((c) => c.modality === "scalar");
    const digitalCh = channels.find((c) => c.modality === "digital_bool");
    const fsmCh = channels.find((c) => c.modality === "state_machine");
    assert.ok(analogCh && digitalCh && fsmCh);

    // Add a 3rd standalone MSO scope with customized tracks, trigger, cursors & sync group
    const scope3Id = store.getState().addScope("Custom Standalone Scope 3");
    store.getState().bindChannelToScope(scope3Id, digitalCh.channel_id);
    store.getState().bindChannelToScope(scope3Id, fsmCh.channel_id);

    const analogHex = hexToRgbaNumber("#f97316");
    const digitalHex = hexToRgbaNumber("#22d3ee");
    const fsmHex = hexToRgbaNumber("#ec4899");

    store.getState().updateScope(scope3Id, (prev) => ({
      ...prev,
      timeSpanNs: 5_000_000,
      yAxisScale: 42,
      yAxisOffset: 12,
      wheelZoomTarget: "y",
      syncGroupId: 1,
      cursorsEnabled: true,
      cursorARatio: 0.2,
      cursorBRatio: 0.8,
      trigger: {
        kind: "edge",
        channel_id: analogCh.channel_id,
        level: 9,
        hysteresis: 0.5,
        slope: "rising",
      },
      tracks: prev.tracks.map((tr) => {
        if (tr.channelId === analogCh.channel_id) {
          return { ...tr, colorRgba: analogHex, yScale: 1.5, yOffset: -2 };
        }
        if (tr.channelId === digitalCh.channel_id) {
          return { ...tr, colorRgba: digitalHex };
        }
        return { ...tr, colorRgba: fsmHex };
      }),
    }));

    // Add a 2nd canvas (locked) containing an embedded mso_scope + digital_meter + slider + cmd_button
    const canvas2Id = store.getState().addCanvas("Custom Motor Board");
    const scopeWidgetId = store.getState().addWidgetToCanvas(canvas2Id, "mso_scope", {
      col: 0,
      row: 0,
      channelId: analogCh.channel_id,
    });
    store.getState().bindChannelToWidget(canvas2Id, scopeWidgetId, digitalCh.channel_id);
    store.getState().bindChannelToWidget(canvas2Id, scopeWidgetId, fsmCh.channel_id);

    const meterWidgetId = store.getState().addWidgetToCanvas(canvas2Id, "digital_meter", {
      col: 2,
      row: 8,
      channelId: analogCh.channel_id,
    });
    const sliderWidgetId = store.getState().addWidgetToCanvas(canvas2Id, "param_slider", {
      col: 10,
      row: 8,
    });
    store.getState().toggleCanvasLock(canvas2Id);

    const embWidget = store
      .getState()
      .canvases[canvas2Id].widgets.find((w) => w.id === scopeWidgetId);
    assert.ok(embWidget && embWidget.scopeId !== null);
    const origEmbScopeId = embWidget.scopeId;

    store.getState().updateScope(origEmbScopeId, (prev) => ({
      ...prev,
      yAxisScale: 60,
      yAxisOffset: -5,
      tracks: prev.tracks.map((tr) =>
        tr.channelId === fsmCh.channel_id ? { ...tr, colorRgba: fsmHex } : tr,
      ),
    }));

    // Customize page chrome & downlink bar controls
    store.getState().setMockRateHz(60);
    store.getState().setSidebarCollapsed(true);
    store.getState().setSidebarTab("toolbox");
    store.getState().setDownlinkBarCollapsed(true);
    store.getState().setFocusedScopeId(scope3Id);
    store.getState().setActiveCanvasId(canvas2Id);
    store.getState().updateDownlinkConfig({
      targetSource: "sim0",
      priority: "high",
      manualMode: "hex_bytes",
      asciiText: "PING",
      appendNewline: false,
      hexText: "DE AD BE EF",
      kvKey: "torque",
      kvVal: "3.5",
    });

    // Register a mock Dockview split layout matching all open panels
    const mockedDockviewLayout: Record<string, unknown> = {
      grid: {
        root: {
          type: "branch",
          data: [
            { type: "leaf", data: { id: "g1", views: ["scope-1", "scope-2", `scope-${scope3Id}`], activeView: `scope-${scope3Id}` }, size: 600 },
            { type: "leaf", data: { id: "g2", views: ["canvas-1", `canvas-${canvas2Id}`], activeView: `canvas-${canvas2Id}` }, size: 400 },
          ],
        },
        width: 1000,
        height: 700,
        orientation: "HORIZONTAL",
      },
      panels: {
        "scope-1": { id: "scope-1", contentComponent: "msoScope", params: { scopeId: 1 } },
        "scope-2": { id: "scope-2", contentComponent: "msoScope", params: { scopeId: 2 } },
        [`scope-${scope3Id}`]: {
          id: `scope-${scope3Id}`,
          contentComponent: "msoScope",
          params: { scopeId: scope3Id },
        },
        "canvas-1": { id: "canvas-1", contentComponent: "dashboardCanvas", params: { canvasId: 1 } },
        [`canvas-${canvas2Id}`]: {
          id: `canvas-${canvas2Id}`,
          contentComponent: "dashboardCanvas",
          params: { canvasId: canvas2Id },
        },
      },
      activeGroup: "g2",
    };
    store.getState().registerDockviewLayoutAccessor(() => mockedDockviewLayout);

    // Export entire page board config
    const exportedBoardJson = store.getState().exportBoardConfig();
    const parsedBoard = parseBoardConfigJson(exportedBoardJson);
    assert.equal(parsedBoard.schema, BOARD_CONFIG_SCHEMA_V1);
    assert.equal(parsedBoard.standaloneScopes.length, 3);
    assert.equal(parsedBoard.canvases.length, 2);
    assert.equal(parsedBoard.ui.mockRateHz, 60);
    assert.equal(parsedBoard.ui.sidebarCollapsed, true);
    assert.equal(parsedBoard.ui.sidebarTab, "toolbox");
    assert.equal(parsedBoard.ui.downlinkBarCollapsed, true);
    assert.equal(parsedBoard.downlink.manualMode, "hex_bytes");
    assert.equal(parsedBoard.downlink.hexText, "DE AD BE EF");
    assert.ok(parsedBoard.dockviewLayout !== null);

    // Mutate workspace heavily & replace sim0 so numeric channel IDs shift upward
    store.getState().registerDockviewLayoutAccessor(null);
    store.getState().removeScope(scope3Id);
    store.getState().removeCanvas(canvas2Id);
    store.getState().clearCanvasWidgets(1);
    store.getState().setMockRateHz(120);
    store.getState().setSidebarCollapsed(false);
    store.getState().setSidebarTab("sources");
    store.getState().setDownlinkBarCollapsed(false);
    store.getState().updateDownlinkConfig({
      priority: "normal",
      manualMode: "ascii_text",
      hexText: "00 00",
    });

    const seqBeforeImport = store.getState().layoutRestoreSeq;
    const sourcesJson = store.getState().exportSourcesConfig();
    await store.getState().importSourcesConfig(sourcesJson, { replaceExisting: true });

    // Import whole-page board config back
    const importRes = await store.getState().importBoardConfig(exportedBoardJson);
    assert.deepEqual(importRes.missingChannelPaths, []);
    assert.equal(importRes.scopeCount, 3);
    assert.equal(importRes.canvasCount, 2);

    const restored = store.getState();
    assert.deepEqual(restored.scopeOrder, [1, 2, scope3Id]);
    assert.deepEqual(restored.canvasOrder, [1, canvas2Id]);
    assert.equal(restored.layoutRestoreSeq, seqBeforeImport + 1);
    assert.ok(restored.pendingDockviewLayout !== null);
    assert.equal(
      (restored.pendingDockviewLayout!.panels as Record<string, { title?: string }>)[
        `scope-${scope3Id}`
      ].title,
      "Custom Standalone Scope 3",
    );

    // UI & Downlink settings restored
    assert.equal(restored.mockRateHz, 60);
    assert.equal(restored.sidebarCollapsed, true);
    assert.equal(restored.sidebarTab, "toolbox");
    assert.equal(restored.downlinkBarCollapsed, true);
    assert.equal(restored.focusedScopeId, scope3Id);
    assert.equal(restored.activeCanvasId, canvas2Id);
    assert.equal(restored.downlinkConfig.priority, "high");
    assert.equal(restored.downlinkConfig.manualMode, "hex_bytes");
    assert.equal(restored.downlinkConfig.hexText, "DE AD BE EF");
    assert.equal(restored.downlinkConfig.asciiText, "PING");
    assert.equal(restored.downlinkConfig.appendNewline, false);
    assert.equal(restored.downlinkConfig.kvKey, "torque");
    assert.equal(restored.downlinkConfig.kvVal, "3.5");

    // Verify shifted channels resolved by full_path across standalone scopes & canvas widgets
    const rehydratedAnalog = Object.values(restored.channelsById).find(
      (c) => c.full_path === analogCh.full_path,
    );
    const rehydratedDigital = Object.values(restored.channelsById).find(
      (c) => c.full_path === digitalCh.full_path,
    );
    const rehydratedFsm = Object.values(restored.channelsById).find(
      (c) => c.full_path === fsmCh.full_path,
    );
    assert.ok(rehydratedAnalog && rehydratedDigital && rehydratedFsm);
    assert.notEqual(rehydratedAnalog.channel_id, analogCh.channel_id);

    const restoredScope3 = restored.scopes[scope3Id];
    assert.equal(restoredScope3.timeSpanNs, 5_000_000);
    assert.equal(restoredScope3.yAxisScale, 42);
    assert.equal(restoredScope3.yAxisOffset, 12);
    assert.equal(restoredScope3.wheelZoomTarget, "y");
    assert.equal(restoredScope3.syncGroupId, 1);
    assert.equal(restoredScope3.cursorsEnabled, true);
    assert.equal(restoredScope3.trigger?.channel_id, rehydratedAnalog.channel_id);
    assert.equal(
      restoredScope3.tracks.find((t) => t.channelId === rehydratedAnalog.channel_id)?.colorRgba,
      analogHex,
    );
    assert.equal(
      restoredScope3.tracks.find((t) => t.channelId === rehydratedDigital.channel_id)?.colorRgba,
      digitalHex,
    );
    assert.equal(
      restoredScope3.tracks.find((t) => t.channelId === rehydratedFsm.channel_id)?.colorRgba,
      fsmHex,
    );

    const restoredCanvas2 = restored.canvases[canvas2Id];
    assert.equal(restoredCanvas2.locked, true);
    assert.equal(restoredCanvas2.widgets.length, 3);
    const restoredMeter = restoredCanvas2.widgets.find((w) => w.id === meterWidgetId);
    const restoredSlider = restoredCanvas2.widgets.find((w) => w.id === sliderWidgetId);
    const restoredEmbWidget = restoredCanvas2.widgets.find((w) => w.type === "mso_scope");
    assert.ok(restoredMeter && restoredSlider && restoredEmbWidget);
    assert.equal(restoredMeter.col, 2);
    assert.equal(restoredMeter.row, 8);
    assert.equal(restoredMeter.primaryBinding?.channelId, rehydratedAnalog.channel_id);
    assert.ok(restoredEmbWidget.scopeId !== null);
    assert.equal(restored.scopeOrder.includes(restoredEmbWidget.scopeId), false);
    const restoredEmbScope = restored.scopes[restoredEmbWidget.scopeId];
    assert.equal(restoredEmbScope.yAxisScale, 60);
    assert.equal(restoredEmbScope.yAxisOffset, -5);
    assert.equal(
      restoredEmbScope.tracks.find((t) => t.channelId === rehydratedFsm.channel_id)?.colorRgba,
      fsmHex,
    );

    // Guard check: missing channelPath must NOT falsely bind via numeric fallbackChannelId,
    // and invalid dockviewLayout referencing non-existent panels sanitizes cleanly to null
    const collisionFile = parseBoardConfigJson(exportedBoardJson);
    collisionFile.standaloneScopes[0].config.tracks = [
      {
        channelPath: "ghost_source/nonexistent_ch",
        fallbackChannelId: rehydratedAnalog.channel_id,
        lane: "analog",
        visible: true,
        yScale: 1,
        yOffset: 0,
        colorRgba: analogHex,
      },
    ];
    collisionFile.canvases[1].widgets[1].primaryBinding = {
      channelPath: "ghost_source/nonexistent_ch",
      fallbackChannelId: rehydratedAnalog.channel_id,
      scale: 1,
      offset: 0,
    };
    collisionFile.dockviewLayout = {
      ...(collisionFile.dockviewLayout ?? {}),
      panels: {
        "scope-999": { id: "scope-999", contentComponent: "msoScope", params: { scopeId: 999 } },
      },
    };

    const rehydratedMissing = rehydrateBoardFromConfig({
      file: collisionFile,
      channelsById: restored.channelsById,
      availableSourceIds: ["sim0"],
    });
    assert.deepEqual(rehydratedMissing.missingChannelPaths, ["ghost_source/nonexistent_ch"]);
    assert.deepEqual(rehydratedMissing.scopes[1].tracks, []);
    assert.equal(rehydratedMissing.canvases[canvas2Id].widgets[1].primaryBinding, null);
    assert.equal(rehydratedMissing.dockviewLayout, null);
  } finally {
    client.stopDataPlane();
  }
});

test("Cross-schema config loaders reject mismatched configs and upgrade legacy single-canvas files", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    const sourcesJson = store.getState().exportSourcesConfig();
    const boardJson = store.getState().exportBoardConfig();

    assert.throws(
      () => parseSourcesConfigJson(boardJson),
      /ddscope\.sources\.v1.*ddscope\.board\.v1/,
    );
    assert.throws(
      () => parseBoardConfigJson(sourcesJson),
      /ddscope\.board\.v1.*ddscope\.sources\.v1/,
    );
    await assert.rejects(
      () => store.getState().importSourcesConfig(boardJson),
      /ddscope\.sources\.v1/,
    );
    await assert.rejects(
      () => store.getState().importBoardConfig(sourcesJson),
      /ddscope\.board\.v1/,
    );

    // Legacy ddscope.canvas.v1 files upgrade seamlessly into a 1-canvas whole-page board
    const legacySingleCanvasJson = JSON.stringify({
      schema: CANVAS_CONFIG_SCHEMA_V1,
      exportedAt: new Date().toISOString(),
      canvas: {
        title: "Legacy Canvas",
        locked: true,
        widgets: [
          {
            type: "digital_meter",
            title: "sim0/ia",
            col: 0,
            row: 0,
            colSpan: 8,
            rowSpan: 4,
            primaryBinding: { channelPath: "sim0/ia", scale: 1, offset: 0 },
            bindings: [],
          },
        ],
      },
    });
    const legacyRes = await store.getState().importBoardConfig(legacySingleCanvasJson);
    assert.equal(legacyRes.scopeCount, 0);
    assert.equal(legacyRes.canvasCount, 1);
    assert.equal(legacyRes.widgetCount, 1);
    assert.deepEqual(legacyRes.missingChannelPaths, []);
    assert.equal(store.getState().canvases[1].title, "Legacy Canvas");
    assert.equal(store.getState().canvases[1].locked, true);
  } finally {
    client.stopDataPlane();
  }
});

test("Project config exports and restores both data sources and whole-page board layout", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();

    await store.getState().createSource({
      source_id: "udp_proj",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:19100",
        remote_addr: "192.168.1.50:19101",
      },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 10000,
        auto_estimate_rate: false,
      },
      protocol: {
        codec: "fire_water",
        channel_names: ["alpha", "beta"],
      },
      auto_start: false,
    });

    const alphaCh = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "udp_proj/alpha",
    );
    assert.ok(alphaCh, "expected udp_proj/alpha channel");

    const canvasId = store.getState().addCanvas("Project Bench");
    const widgetId = store.getState().addWidgetToCanvas(canvasId, "digital_meter", {
      col: 0,
      row: 0,
      channelId: alphaCh.channel_id,
    });
    store.getState().bindChannelToWidget(canvasId, widgetId, alphaCh.channel_id);
    store.getState().setMockRateHz(60);
    store.getState().updateDownlinkConfig({
      targetSource: "udp_proj",
      asciiText: "PING_PROJ",
    });

    const projectJson = store.getState().exportProjectConfig();
    const parsedProject = parseProjectConfigJson(projectJson);
    assert.equal(parsedProject.schema, PROJECT_CONFIG_SCHEMA_V1);
    assert.equal(parsedProject.sources.some((s) => s.source_id === "udp_proj"), true);
    assert.equal(
      parsedProject.board.canvases.some((c) => c.title === "Project Bench"),
      true,
    );

    // Mutate workspace completely: remove udp_proj, add a temporary source, delete custom canvas
    await store.getState().removeSource("udp_proj");
    await store.getState().createSource({
      source_id: "temp_other",
      transport: { kind: "sim", preset: "motor_foc", sample_rate_hz: 1000 },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 1000,
        auto_estimate_rate: false,
      },
      protocol: { codec: "just_float", channel_names: [] },
      auto_start: false,
    });
    store.getState().removeCanvas(canvasId);
    assert.equal(
      store.getState().channelGroups.some((g) => g.source.source_id === "udp_proj"),
      false,
    );

    // Restore full project config
    const res = await store.getState().importProjectConfig(projectJson);
    assert.equal(res.sourceCount, parsedProject.sources.length);
    assert.deepEqual(res.missingChannelPaths, []);

    const restored = store.getState();
    assert.equal(
      restored.channelGroups.some((g) => g.source.source_id === "udp_proj"),
      true,
    );
    assert.equal(
      restored.channelGroups.some((g) => g.source.source_id === "temp_other"),
      false,
    );
    assert.equal(restored.mockRateHz, 60);
    assert.equal(restored.downlinkConfig.targetSource, "udp_proj");
    assert.equal(restored.downlinkConfig.asciiText, "PING_PROJ");

    const restoredCanvas = Object.values(restored.canvases).find(
      (c) => c.title === "Project Bench",
    );
    assert.ok(restoredCanvas);
    assert.equal(restoredCanvas.widgets.length, 1);
    const restoredAlpha = Object.values(restored.channelsById).find(
      (c) => c.full_path === "udp_proj/alpha",
    );
    assert.ok(restoredAlpha);
    assert.equal(
      restoredCanvas.widgets[0].primaryBinding?.channelId,
      restoredAlpha.channel_id,
    );
  } finally {
    client.stopDataPlane();
  }
});

test("Project config parser rejects sources and board schemas and vice versa", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    const sourcesJson = store.getState().exportSourcesConfig();
    const boardJson = store.getState().exportBoardConfig();
    const projectJson = store.getState().exportProjectConfig();

    assert.throws(
      () => parseProjectConfigJson(sourcesJson),
      /ddscope\.project\.v1.*ddscope\.sources\.v1/,
    );
    assert.throws(
      () => parseProjectConfigJson(boardJson),
      /ddscope\.project\.v1.*ddscope\.board\.v1/,
    );
    assert.throws(
      () => parseSourcesConfigJson(projectJson),
      /ddscope\.sources\.v1.*ddscope\.project\.v1/,
    );
    assert.throws(
      () => parseBoardConfigJson(projectJson),
      /ddscope\.board\.v1.*ddscope\.project\.v1/,
    );
  } finally {
    client.stopDataPlane();
  }
});

test("saveDefaultProjectConfig stores portable default and initWorkspace auto-loads it on startup", async () => {
  let portableDefaultFile: string | null = null;
  const mockPath = "D:\\portable\\DDScopeHost\\ddscope.default-project.json";

  const makeMockAdapter = () => {
    const bus = new HighFreqTopicBus();
    const mockClient = new BridgeClient(bus);
    const mockAdapter = {
      isTauriHost: () => true,
      commands: {
        getRuntimeStatus: () => mockClient.getRuntimeStatus(),
        getChannelTree: () => mockClient.getChannelTree(),
        createSource: (req: Parameters<BridgeClient["createSource"]>[0]) =>
          mockClient.createSource(req),
        getSourceConfig: (id: string) => mockClient.getSourceConfig(id),
        startSource: (id: string) => mockClient.startSource(id),
        stopSource: (id: string) => mockClient.stopSource(id),
        removeSource: (id: string) => mockClient.removeSource(id),
        setRawRecorder: (req: Parameters<BridgeClient["setRawRecorder"]>[0]) =>
          mockClient.setRawRecorder(req),
        seekReplaySource: (req: Parameters<BridgeClient["seekReplaySource"]>[0]) =>
          mockClient.seekReplaySource(req),
        upsertScopeViewport: (cfg: Parameters<BridgeClient["upsertScopeViewport"]>[0]) =>
          mockClient.upsertScopeViewport(cfg),
        removeScopeViewport: (id: number) => mockClient.removeScopeViewport(id),
        sendDownlink: (req: Parameters<BridgeClient["sendDownlink"]>[0]) =>
          mockClient.sendDownlink(req),
        loadDefaultProjectConfig: async () => portableDefaultFile,
        saveDefaultProjectConfig: async (jsonText: string) => {
          portableDefaultFile = jsonText;
          return mockPath;
        },
        clearDefaultProjectConfig: async () => {
          portableDefaultFile = null;
          return true;
        },
      },
    };
    return { bus, mockClient, mockAdapter };
  };

  const session1 = makeMockAdapter();
  const client1 = new BridgeClient(session1.bus, session1.mockAdapter);
  const store1 = createWorkspaceStore(client1);

  try {
    await store1.getState().initWorkspace();
    await store1.getState().createSource({
      source_id: "default_sim_custom",
      transport: { kind: "sim", preset: "motor_foc", sample_rate_hz: 5000 },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 5000,
        auto_estimate_rate: false,
      },
      protocol: { codec: "just_float", channel_names: [] },
      auto_start: false,
    });
    store1.getState().addCanvas("Startup Default Canvas");
    store1.getState().setMockRateHz(60);

    const saved = await store1.getState().saveDefaultProjectConfig();
    assert.equal(saved.mode, "exe-dir");
    assert.equal(saved.location, mockPath);
    assert.ok(portableDefaultFile);
  } finally {
    client1.stopDataPlane();
    session1.mockClient.stopDataPlane();
  }

  // Second startup with a brand-new client & store automatically loads portableDefaultFile
  const session2 = makeMockAdapter();
  const client2 = new BridgeClient(session2.bus, session2.mockAdapter);
  const store2 = createWorkspaceStore(client2);

  try {
    await store2.getState().initWorkspace();
    const s2 = store2.getState();
    assert.equal(s2.initialized, true);
    assert.equal(
      s2.channelGroups.some((g) => g.source.source_id === "default_sim_custom"),
      true,
    );
    assert.equal(s2.mockRateHz, 60);
    assert.equal(
      Object.values(s2.canvases).some((c) => c.title === "Startup Default Canvas"),
      true,
    );
  } finally {
    client2.stopDataPlane();
    session2.mockClient.stopDataPlane();
  }
});
