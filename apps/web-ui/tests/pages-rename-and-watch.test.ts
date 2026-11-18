import test from "node:test";
import assert from "node:assert/strict";
import { BridgeClient, HighFreqTopicBus } from "../src/bridge/index.ts";
import {
  parseBoardConfigJson,
  parseProjectConfigJson,
} from "../src/workspace/config-io.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("Watch Table and Scope sequentially add multiple channels without duplicates while preserving custom scaling", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();

    const channels = Object.values(store.getState().channelsById);
    const scalars = channels.filter((c) => c.modality === "scalar");
    const digitals = channels.filter((c) => c.modality === "digital_bool");
    assert.ok(scalars.length >= 2 && digitals.length >= 1);

    const cid = store.getState().activeCanvasId!;
    // Start with an empty watch_table and add 3 distinct channels sequentially
    const watchId = store.getState().addWidgetToCanvas(cid, "watch_table");
    store.getState().bindChannelToWidget(cid, watchId, scalars[0].channel_id);

    // Customize scale/offset on the 1st row to confirm later appends preserve it
    store.getState().updateCanvasWidget(cid, watchId, (prev) => ({
      ...prev,
      bindings: prev.bindings.map((b) =>
        b.channelId === scalars[0].channel_id ? { ...b, scale: 2.5, offset: -1 } : b,
      ),
    }));

    store.getState().bindChannelToWidget(cid, watchId, scalars[1].channel_id);
    store.getState().bindChannelToWidget(cid, watchId, digitals[0].channel_id);
    // Re-adding an already bound channel must be a no-op duplicate guard
    store.getState().bindChannelToWidget(cid, watchId, scalars[0].channel_id);

    const watch = store.getState().canvases[cid].widgets.find((w) => w.id === watchId)!;
    assert.equal(watch.bindings.length, 3);
    assert.equal(watch.bindings[0].scale, 2.5);
    assert.equal(watch.bindings[0].offset, -1);

    // Also add channels sequentially to Scope 1 via bindChannelToScope
    const beforeTrackCount = store.getState().scopes[1].tracks.length;
    const unbound = channels.find(
      (c) => !store.getState().scopes[1].tracks.some((tr) => tr.channelId === c.channel_id),
    )!;
    assert.ok(unbound);
    store.getState().bindChannelToScope(1, unbound.channel_id);
    assert.equal(store.getState().scopes[1].tracks.length, beforeTrackCount + 1);
  } finally {
    client.stopDataPlane();
  }
});

test("renameScope, renameCanvas, widget title rename, and renamePage survive locale switches while defaults translate", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    store.getState().setLocale("en");

    // Scope 1 has default EN title; add Scope 3 (untouched default) & rename Scope 2
    const s3Id = store.getState().addScope();
    assert.equal(store.getState().scopes[s3Id].title, `MSO Scope ${s3Id}`);

    store.getState().renameScope(2, "Bus Surge Trap");
    // Empty whitespace rename must be ignored
    store.getState().renameScope(2, "   ");
    assert.equal(store.getState().scopes[2].title, "Bus Surge Trap");

    // Rename Canvas 1 and add Canvas 2 with untouched default title
    const c2Id = store.getState().addCanvas();
    assert.equal(store.getState().canvases[c2Id].title, `Instrument Canvas ${c2Id}`);
    store.getState().renameCanvas(1, "Motor Telemetry");

    // Add embedded mso_scope widget & sync rename
    const embWidgetId = store.getState().addWidgetToCanvas(1, "mso_scope");
    const embScopeId = store
      .getState()
      .canvases[1].widgets.find((w) => w.id === embWidgetId)!.scopeId!;
    store.getState().updateCanvasWidget(1, embWidgetId, (prev) => ({
      ...prev,
      title: "Gate Drive Scope",
    }));
    store.getState().renameScope(embScopeId, "Gate Drive Scope");

    // Add Page 2 (untouched default) & rename Page 1
    const p2Id = store.getState().addPage();
    assert.equal(store.getState().pages[p2Id].title, `Page ${p2Id}`);
    store.getState().renamePage(1, "Power Stage");

    // Switch locale EN -> ZH-CN
    store.getState().setLocale("zh-CN");
    assert.equal(store.getState().scopes[2].title, "Bus Surge Trap");
    assert.equal(store.getState().scopes[s3Id].title, `MSO 示波器 ${s3Id}`);
    assert.equal(store.getState().canvases[1].title, "Motor Telemetry");
    assert.equal(store.getState().canvases[c2Id].title, `测控仪表画布 ${c2Id}`);
    assert.equal(store.getState().scopes[embScopeId].title, "Gate Drive Scope");
    assert.equal(store.getState().pages[1].title, "Power Stage");
    assert.equal(store.getState().pages[p2Id].title, `页面 ${p2Id}`);

    // Switch back ZH-CN -> EN
    store.getState().setLocale("en");
    assert.equal(store.getState().pages[1].title, "Power Stage");
    assert.equal(store.getState().pages[p2Id].title, `Page ${p2Id}`);
    assert.equal(store.getState().scopes[s3Id].title, `MSO Scope ${s3Id}`);
  } finally {
    client.stopDataPlane();
  }
});

test("Workspace big pages lifecycle: addPage, switch, removePage cleanup, and full Board + Project persistence roundtrip", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    store.getState().setLocale("en");

    // Customize Page 1
    store.getState().renamePage(1, "Live Capture");
    store.getState().renameScope(1, "Phase Current");
    store.getState().renameCanvas(1, "Bench Overview");

    const ch0 = Object.values(store.getState().channelsById).find(
      (c) => c.modality === "scalar",
    )!;
    assert.ok(ch0);

    // Create Page 2 ("Diagnostics") with its own Scope, Canvas, and embedded mso_scope widget
    const p2Id = store.getState().addPage("Diagnostics");
    assert.equal(store.getState().activePageId, p2Id);
    assert.deepEqual(store.getState().scopeOrder, []);
    assert.deepEqual(store.getState().canvasOrder, []);

    const p2ScopeId = store.getState().addScope("Page2 FFT View");
    store.getState().bindChannelToScope(p2ScopeId, ch0.channel_id);
    const p2CanvasId = store.getState().addCanvas("Page2 Controls");
    const p2EmbWidgetId = store.getState().addWidgetToCanvas(p2CanvasId, "mso_scope", {
      channelId: ch0.channel_id,
    });
    const p2EmbScopeId = store
      .getState()
      .canvases[p2CanvasId].widgets.find((w) => w.id === p2EmbWidgetId)!.scopeId!;

    assert.deepEqual(store.getState().scopeOrder, [p2ScopeId]);
    assert.deepEqual(store.getState().canvasOrder, [p2CanvasId]);

    // Switch back to Page 1 -> restores Page 1's scopes/canvases without losing Page 2's state
    store.getState().setActivePage(1);
    assert.equal(store.getState().activePageId, 1);
    assert.deepEqual(store.getState().scopeOrder, [1, 2]);
    assert.deepEqual(store.getState().canvasOrder, [1]);
    assert.deepEqual(store.getState().pages[p2Id].scopeOrder, [p2ScopeId]);
    assert.deepEqual(store.getState().pages[p2Id].canvasOrder, [p2CanvasId]);

    // Export full Project JSON while Page 2 is active and restore from clean clear
    store.getState().setActivePage(p2Id);
    const boardJson = store.getState().exportBoardConfig();
    const projectJson = store.getState().exportProjectConfig();

    const parsedBoard = parseBoardConfigJson(boardJson);
    assert.equal(parsedBoard.pages?.length, 2);
    assert.equal(parsedBoard.ui.activePageId, p2Id);

    const parsedProj = parseProjectConfigJson(projectJson);
    assert.equal(parsedProj.board.pages?.length, 2);

    // Create a 3rd temporary page, then delete Page 3 and confirm scopes/canvases cleanup
    const p3Id = store.getState().addPage("Temp Page");
    const p3Scope = store.getState().addScope("Temp Scope");
    const p3Canvas = store.getState().addCanvas("Temp Canvas");
    const p3EmbWidget = store.getState().addWidgetToCanvas(p3Canvas, "mso_scope");
    const p3EmbScope = store
      .getState()
      .canvases[p3Canvas].widgets.find((w) => w.id === p3EmbWidget)!.scopeId!;

    store.getState().removePage(p3Id);
    assert.equal(store.getState().pages[p3Id], undefined);
    assert.equal(store.getState().scopes[p3Scope], undefined);
    assert.equal(store.getState().scopes[p3EmbScope], undefined);
    assert.equal(store.getState().canvases[p3Canvas], undefined);
    assert.equal(store.getState().activePageId, p2Id);

    // Import exported Project JSON and verify both pages & custom titles return cleanly
    await store.getState().importProjectConfig(projectJson);
    assert.equal(store.getState().pageOrder.length, 2);
    assert.equal(store.getState().activePageId, p2Id);
    assert.equal(store.getState().pages[1].title, "Live Capture");
    assert.equal(store.getState().pages[p2Id].title, "Diagnostics");
    assert.equal(store.getState().scopes[1].title, "Phase Current");
    assert.equal(store.getState().canvases[1].title, "Bench Overview");
    assert.equal(store.getState().scopes[p2ScopeId].title, "Page2 FFT View");
    assert.ok(store.getState().scopes[p2EmbScopeId]);

    // Deleting Page 2 leaves Page 1 active; attempting to delete the sole remaining Page 1 is guarded
    store.getState().removePage(p2Id);
    assert.deepEqual(store.getState().pageOrder, [1]);
    assert.equal(store.getState().activePageId, 1);
    store.getState().removePage(1);
    assert.deepEqual(store.getState().pageOrder, [1]);
    assert.ok(store.getState().pages[1]);
  } finally {
    client.stopDataPlane();
  }
});
