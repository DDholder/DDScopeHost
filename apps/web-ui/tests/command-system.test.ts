import test from "node:test";
import assert from "node:assert/strict";
import type { SendDownlinkRequestDto_Deserialize } from "../src/bridge/bindings.ts";
import { BridgeClient, HighFreqTopicBus } from "../src/bridge/index.ts";
import {
  COMMANDS_CONFIG_SCHEMA_V1,
  parseBoardConfigJson,
  parseCommandsConfigJson,
  parseProjectConfigJson,
  parseSourcesConfigJson,
} from "../src/workspace/config-io.ts";
import {
  buildEffectiveCommandArgs,
  commandUsesValue,
  compileCmdTableRow,
  compileCommandPayload,
  countCommandPlaceholders,
  createCmdTableRow,
  extractCommandPlaceholders,
  updateCmdTableRowValue,
} from "../src/workspace/command-system.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("compileCommandPayload formats static and parameterized String and Hex commands", () => {
  // Static Hex variations normalize to uppercase space-separated bytes
  for (const tpl of ["00 01 02", "0x00, 0x01, 0x02", "000102"]) {
    const res = compileCommandPayload(
      { format: "hex", template: tpl, appendNewline: false, endian: "little" },
      0,
    );
    assert.equal(res.ok, true);
    assert.equal(res.usesValue, false);
    if (res.ok) {
      assert.deepEqual(res.payload, { kind: "hex_bytes", hex: "00 01 02" });
      assert.equal(res.preview, "HEX: 00 01 02");
    }
  }

  // Invalid Hex rejected cleanly
  for (const badHex of ["00 1", "GG", "   "]) {
    const res = compileCommandPayload(
      { format: "hex", template: badHex, appendNewline: false, endian: "little" },
      0,
    );
    assert.equal(res.ok, false);
  }

  // Static String respects appendNewline
  const strNoNl = compileCommandPayload(
    { format: "string", template: "123", appendNewline: false, endian: "little" },
    0,
  );
  assert.deepEqual(strNoNl, {
    ok: true,
    usesValue: false,
    preview: "123",
    payload: { kind: "ascii_text", text: "123", append_newline: false },
  });

  const strWithNl = compileCommandPayload(
    { format: "string", template: "123", appendNewline: true, endian: "little" },
    0,
  );
  assert.deepEqual(strWithNl, {
    ok: true,
    usesValue: false,
    preview: "123\\n",
    payload: { kind: "ascii_text", text: "123", append_newline: true },
  });

  // Formatted String (%f clean formatting, precision, %02X, %d, %%)
  const setNum = compileCommandPayload(
    { format: "string", template: "set_num %f", appendNewline: false, endian: "little" },
    2.5,
  );
  assert.deepEqual(setNum, {
    ok: true,
    usesValue: true,
    preview: "set_num 2.5",
    payload: { kind: "ascii_text", text: "set_num 2.5", append_newline: false },
  });

  const floatNoise = compileCommandPayload(
    { format: "string", template: "set_num %f", appendNewline: false, endian: "little" },
    0.1 + 0.2,
  );
  assert.equal(floatNoise.ok, true);
  if (floatNoise.ok) {
    assert.deepEqual(floatNoise.payload, {
      kind: "ascii_text",
      text: "set_num 0.3",
      append_newline: false,
    });
  }

  const multiFmt = compileCommandPayload(
    {
      format: "string",
      template: "kp=%.2f, idx=%02X, cnt=%d %%",
      appendNewline: false,
      endian: "little",
    },
    15.75,
  );
  assert.equal(multiFmt.ok, true);
  if (multiFmt.ok) {
    assert.deepEqual(multiFmt.payload, {
      kind: "ascii_text",
      text: "kp=15.75, idx=0F, cnt=15 %",
      append_newline: false,
    });
  }

  // Invalid String specifier fails gracefully
  const badFmt = compileCommandPayload(
    { format: "string", template: "bad %q", appendNewline: false, endian: "little" },
    1,
  );
  assert.equal(badFmt.ok, false);

  // Parameterized Hex (%f LE / BE, %d, %hu, %02X)
  const hexFloatLe = compileCommandPayload(
    { format: "hex", template: "AA 55 %f", appendNewline: false, endian: "little" },
    1.0,
  );
  assert.deepEqual(hexFloatLe, {
    ok: true,
    usesValue: true,
    preview: "HEX: AA 55 00 00 80 3F",
    payload: { kind: "hex_bytes", hex: "AA 55 00 00 80 3F" },
  });

  const hexFloatBe = compileCommandPayload(
    { format: "hex", template: "AA 55 %f", appendNewline: false, endian: "big" },
    1.0,
  );
  assert.deepEqual(hexFloatBe, {
    ok: true,
    usesValue: true,
    preview: "HEX: AA 55 3F 80 00 00",
    payload: { kind: "hex_bytes", hex: "AA 55 3F 80 00 00" },
  });

  const hexMixed = compileCommandPayload(
    { format: "hex", template: "01 %02X %hu %d", appendNewline: false, endian: "little" },
    260,
  );
  // 260 & 0xff = 0x04; 260 as LE u16 = 04 01; 260 as LE i32 = 04 01 00 00
  assert.equal(hexMixed.ok, true);
  if (hexMixed.ok) {
    assert.deepEqual(hexMixed.payload, {
      kind: "hex_bytes",
      hex: "01 04 04 01 04 01 00 00",
    });
  }
});

test("Workspace store manages commands, places/binds widgets, executes formatted payloads, and unbinds on delete", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const sentReqs: SendDownlinkRequestDto_Deserialize[] = [];
  const origSend = client.sendDownlink.bind(client);
  client.sendDownlink = async (req) => {
    sentReqs.push(req);
    return origSend(req);
  };

  const store = createWorkspaceStore(client);
  try {
    await store.getState().initWorkspace();

    const setNumId = store.getState().addCommand({
      name: "设数值",
      format: "string",
      template: "set_num %f",
      appendNewline: false,
      targetSourceId: "sim0",
      priority: "normal",
    });
    const hexId = store.getState().addCommand({
      name: "发Hex",
      format: "hex",
      template: "00 01 02",
      endian: "little",
      targetSourceId: "sim0",
      priority: "high",
    });
    const strId = store.getState().addCommand({
      name: "发字符",
      format: "string",
      template: "123",
      appendNewline: false,
      targetSourceId: "sim0",
      priority: "high",
    });

    assert.equal(commandUsesValue(store.getState().commands[0]), true);
    assert.equal(commandUsesValue(store.getState().commands[1]), false);
    assert.equal(commandUsesValue(store.getState().commands[2]), false);

    const canvasId = store.getState().activeCanvasId!;

    // placeCommandOnCanvas selects param_slider for %f and cmd_button for static commands
    const sliderWid = store.getState().placeCommandOnCanvas(canvasId, setNumId, {
      col: 0,
      row: 12,
    });
    const hexBtnWid = store.getState().placeCommandOnCanvas(canvasId, hexId, {
      col: 12,
      row: 12,
    });

    let canvas = store.getState().canvases[canvasId];
    const placedSlider = canvas.widgets.find((w) => w.id === sliderWid);
    const placedHexBtn = canvas.widgets.find((w) => w.id === hexBtnWid);
    assert.equal(placedSlider?.type, "param_slider");
    assert.equal(placedSlider?.title, "设数值");
    assert.equal(placedSlider?.sliderConfig?.commandId, setNumId);
    assert.equal(placedHexBtn?.type, "cmd_button");
    assert.equal(placedHexBtn?.title, "发Hex");
    assert.equal(placedHexBtn?.buttonConfig?.commandId, hexId);

    // Re-bind an existing cmd_button widget to "发字符"
    store.getState().bindCommandToWidget(canvasId, hexBtnWid, strId);
    canvas = store.getState().canvases[canvasId];
    const reboundBtn = canvas.widgets.find((w) => w.id === hexBtnWid);
    assert.equal(reboundBtn?.title, "发字符");
    assert.equal(reboundBtn?.buttonConfig?.commandId, strId);

    // Execute commands and inspect exact downlink requests
    await store.getState().executeCommand(setNumId, { value: 4.25, isSliderEvent: true });
    await store.getState().executeCommand(hexId);
    await store.getState().executeCommand(strId);

    assert.equal(sentReqs.length, 3);
    assert.deepEqual(sentReqs[0].payload, {
      kind: "ascii_text",
      text: "set_num 4.25",
      append_newline: false,
    });
    assert.deepEqual(sentReqs[1].payload, {
      kind: "hex_bytes",
      hex: "00 01 02",
    });
    assert.deepEqual(sentReqs[2].payload, {
      kind: "ascii_text",
      text: "123",
      append_newline: false,
    });

    // Deleting a command clears commandId on bound widgets
    store.getState().removeCommand(setNumId);
    canvas = store.getState().canvases[canvasId];
    const unboundSlider = canvas.widgets.find((w) => w.id === sliderWid);
    assert.equal(unboundSlider?.sliderConfig?.commandId, null);
    assert.equal(store.getState().commands.some((c) => c.id === setNumId), false);
  } finally {
    client.stopDataPlane();
  }
});

test("Command configs persist across ddscope.commands.v1, board, and project configs with schema guards", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    store.getState().setSidebarTab("commands");

    const setNumId = store.getState().addCommand({
      name: "设数值",
      format: "string",
      template: "set_num %f",
      appendNewline: false,
      priority: "normal",
    });
    const hexId = store.getState().addCommand({
      name: "发Hex",
      format: "hex",
      template: "00 01 02",
      endian: "big",
      priority: "high",
    });

    const canvasId = store.getState().activeCanvasId!;
    const sliderWid = store.getState().placeCommandOnCanvas(canvasId, setNumId);
    const btnWid = store.getState().placeCommandOnCanvas(canvasId, hexId);
    store.getState().updateCanvasWidget(canvasId, btnWid, (prev) => ({
      ...prev,
      buttonConfig: { ...prev.buttonConfig!, commandValue: 7.5 },
    }));

    // 1. Standalone commands export/import round-trip & cross-schema rejection
    const commandsJson = store.getState().exportCommandsConfig();
    const parsedCmds = parseCommandsConfigJson(commandsJson);
    assert.equal(parsedCmds.schema, COMMANDS_CONFIG_SCHEMA_V1);
    assert.equal(parsedCmds.commands.length, 2);

    assert.throws(() => parseSourcesConfigJson(commandsJson), /commands config/i);
    assert.throws(() => parseBoardConfigJson(commandsJson), /commands config/i);
    assert.throws(() => parseProjectConfigJson(commandsJson), /commands config/i);

    const boardJson = store.getState().exportBoardConfig();
    const projectJson = store.getState().exportProjectConfig();
    assert.throws(() => parseCommandsConfigJson(boardJson), /board config/i);
    assert.throws(() => parseCommandsConfigJson(projectJson), /project config/i);

    // 2. Replace commands via import, then merge via import
    const importedReplace = store
      .getState()
      .importCommandsConfig(commandsJson, { replaceExisting: true });
    assert.equal(importedReplace.importedCount, 2);
    assert.equal(store.getState().commands.length, 2);

    // 3. Clear & re-import whole project config; verify sidebarTab, commands, and bindings
    store.getState().removeCommand(setNumId);
    store.getState().removeCommand(hexId);
    store.getState().setSidebarTab("sources");

    await store.getState().importProjectConfig(projectJson);

    const restored = store.getState();
    assert.equal(restored.sidebarTab, "commands");
    assert.equal(restored.commands.length, 2);
    assert.equal(restored.commands[0].name, "设数值");
    assert.equal(restored.commands[0].template, "set_num %f");
    assert.equal(restored.commands[1].name, "发Hex");
    assert.equal(restored.commands[1].endian, "big");

    const restoredCanvas = restored.canvases[canvasId];
    const restoredSlider = restoredCanvas.widgets.find((w) => w.id === sliderWid);
    const restoredBtn = restoredCanvas.widgets.find((w) => w.id === btnWid);
    assert.equal(restoredSlider?.sliderConfig?.commandId, setNumId);
    assert.equal(restoredBtn?.buttonConfig?.commandId, hexId);
    assert.equal(restoredBtn?.buttonConfig?.commandValue, 7.5);
  } finally {
    client.stopDataPlane();
  }
});

test("extractCommandPlaceholders and compileCommandPayload support multi-placeholder String and Hex templates", () => {
  // Sequential String placeholders
  const strTpl = {
    format: "string" as const,
    template: "set_pid %.2f %.3f %d %%",
    appendNewline: false,
    endian: "little" as const,
  };
  assert.deepEqual(extractCommandPlaceholders(strTpl), [
    { index: 0, label: "#1", token: "%.2f" },
    { index: 1, label: "#2", token: "%.3f" },
    { index: 2, label: "#3", token: "%d" },
  ]);
  assert.equal(countCommandPlaceholders(strTpl), 3);

  const strCompiled = compileCommandPayload(strTpl, [1.25, 0.05, -7]);
  assert.deepEqual(strCompiled, {
    ok: true,
    usesValue: true,
    preview: "set_pid 1.25 0.050 -7 %",
    payload: {
      kind: "ascii_text",
      text: "set_pid 1.25 0.050 -7 %",
      append_newline: false,
    },
  });

  // Positional String reuse (%1$d, %1$02X, %2$.1f)
  const posStrTpl = {
    format: "string" as const,
    template: "v=%1$d (0x%1$02X) k=%2$.1f",
    appendNewline: true,
    endian: "little" as const,
  };
  assert.deepEqual(extractCommandPlaceholders(posStrTpl), [
    { index: 0, label: "#1", token: "%1$d" },
    { index: 1, label: "#2", token: "%2$.1f" },
  ]);
  const posCompiled = compileCommandPayload(posStrTpl, [15, 2.5]);
  assert.deepEqual(posCompiled, {
    ok: true,
    usesValue: true,
    preview: "v=15 (0x0F) k=2.5\\n",
    payload: {
      kind: "ascii_text",
      text: "v=15 (0x0F) k=2.5",
      append_newline: true,
    },
  });

  // Sequential Hex placeholders (LE and BE)
  const hexLeTpl = {
    format: "hex" as const,
    template: "AA 55 %hu %f %02X",
    appendNewline: false,
    endian: "little" as const,
  };
  assert.deepEqual(extractCommandPlaceholders(hexLeTpl), [
    { index: 0, label: "#1", token: "%hu" },
    { index: 1, label: "#2", token: "%f" },
    { index: 2, label: "#3", token: "%02X" },
  ]);
  const hexLeCompiled = compileCommandPayload(hexLeTpl, [513, 1.0, 255]);
  // 513 LE u16 = 01 02; 1.0 LE f32 = 00 00 80 3F; 255 = FF
  assert.deepEqual(hexLeCompiled, {
    ok: true,
    usesValue: true,
    preview: "HEX: AA 55 01 02 00 00 80 3F FF",
    payload: {
      kind: "hex_bytes",
      hex: "AA 55 01 02 00 00 80 3F FF",
    },
  });

  const hexBeCompiled = compileCommandPayload(
    { ...hexLeTpl, endian: "big" as const },
    [513, 1.0, 255],
  );
  assert.deepEqual(hexBeCompiled, {
    ok: true,
    usesValue: true,
    preview: "HEX: AA 55 02 01 3F 80 00 00 FF",
    payload: {
      kind: "hex_bytes",
      hex: "AA 55 02 01 3F 80 00 00 FF",
    },
  });

  // buildEffectiveCommandArgs overlays individual slot overrides cleanly
  const effective = buildEffectiveCommandArgs(
    { ...strTpl, argValues: [1.0, 2.0, 3.0] },
    { slotIndex: 1, value: 9.5 },
  );
  assert.deepEqual(effective, [1.0, 9.5, 3.0]);
});

test("Multi-placeholder commands bind sequential widgets, coordinate slot values, and round-trip configs", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const sentReqs: SendDownlinkRequestDto_Deserialize[] = [];
  const origSend = client.sendDownlink.bind(client);
  client.sendDownlink = async (req) => {
    sentReqs.push(req);
    return origSend(req);
  };

  const store = createWorkspaceStore(client);
  try {
    await store.getState().initWorkspace();

    const pidId = store.getState().addCommand({
      name: "PID参数",
      format: "string",
      template: "pid %.2f %.3f %d",
      appendNewline: false,
      targetSourceId: "sim0",
      priority: "normal",
    });

    const canvasId = store.getState().activeCanvasId!;

    // Auto-place 3 widgets sequentially -> binds #1 (slot 0), #2 (slot 1), #3 (slot 2)
    const w1Id = store.getState().placeCommandOnCanvas(canvasId, pidId, { col: 0, row: 12 });
    const w2Id = store.getState().placeCommandOnCanvas(canvasId, pidId, { col: 8, row: 12 });
    const w3Id = store.getState().placeCommandOnCanvas(canvasId, pidId, { col: 16, row: 12 });

    // Place a 4th cmd_button explicitly as Trigger-All (-1) & another cmd_button auto-bound after all slots filled
    const wTrigAllId = store.getState().placeCommandOnCanvas(canvasId, pidId, {
      col: 0,
      row: 16,
      argIndex: -1,
    });
    const wAutoBtnId = store.getState().addWidgetToCanvas(canvasId, "cmd_button", {
      col: 8,
      row: 16,
    });
    store.getState().bindCommandToWidget(canvasId, wAutoBtnId, pidId);

    // Explicitly rebind an extra slider to slot 1 (#2) and preset button on slot 2 (#3)
    const wPresetBtnId = store.getState().addWidgetToCanvas(canvasId, "cmd_button", {
      col: 16,
      row: 16,
    });
    store.getState().updateCanvasWidget(canvasId, wPresetBtnId, (prev) => ({
      ...prev,
      buttonConfig: { ...prev.buttonConfig!, commandValue: 99 },
    }));
    store.getState().bindCommandToWidget(canvasId, wPresetBtnId, pidId, 2);

    let canvas = store.getState().canvases[canvasId];
    const w1 = canvas.widgets.find((w) => w.id === w1Id);
    const w2 = canvas.widgets.find((w) => w.id === w2Id);
    const w3 = canvas.widgets.find((w) => w.id === w3Id);
    const wTrigAll = canvas.widgets.find((w) => w.id === wTrigAllId);
    const wAutoBtn = canvas.widgets.find((w) => w.id === wAutoBtnId);
    const wPresetBtn = canvas.widgets.find((w) => w.id === wPresetBtnId);

    assert.equal(w1?.sliderConfig?.commandArgIndex, 0);
    assert.equal(w1?.title, "PID参数 #1");
    assert.equal(w2?.sliderConfig?.commandArgIndex, 1);
    assert.equal(w2?.title, "PID参数 #2");
    assert.equal(w3?.sliderConfig?.commandArgIndex, 2);
    assert.equal(w3?.title, "PID参数 #3");
    assert.equal(wTrigAll?.type, "cmd_button");
    assert.equal(wTrigAll?.buttonConfig?.commandArgIndex, -1);
    assert.equal(wAutoBtn?.buttonConfig?.commandArgIndex, -1);
    assert.equal(wPresetBtn?.buttonConfig?.commandArgIndex, 2);
    assert.equal(wPresetBtn?.buttonConfig?.commandValue, 99);

    // Disable autoSend on w1 to confirm autoSend flag persists
    store.getState().updateCanvasWidget(canvasId, w1Id, (prev) => ({
      ...prev,
      sliderConfig: { ...prev.sliderConfig!, autoSend: false },
    }));

    // Coordinated updates preserve every slot in every downlink packet
    await store.getState().executeCommand(pidId, {
      argIndex: 0,
      value: 1.5,
      isSliderEvent: true,
    });
    await store.getState().executeCommand(pidId, {
      argIndex: 1,
      value: 0.25,
      isSliderEvent: true,
    });
    await store.getState().executeCommand(pidId, {
      argIndex: 2,
      value: -3,
    });
    await store.getState().executeCommand(pidId, {
      argIndex: -1,
    });

    assert.equal(sentReqs.length, 4);
    assert.deepEqual(sentReqs[0].payload, {
      kind: "ascii_text",
      text: "pid 1.50 1.200 1",
      append_newline: false,
    });
    assert.deepEqual(sentReqs[1].payload, {
      kind: "ascii_text",
      text: "pid 1.50 0.250 1",
      append_newline: false,
    });
    assert.deepEqual(sentReqs[2].payload, {
      kind: "ascii_text",
      text: "pid 1.50 0.250 -3",
      append_newline: false,
    });
    assert.deepEqual(sentReqs[3].payload, {
      kind: "ascii_text",
      text: "pid 1.50 0.250 -3",
      append_newline: false,
    });

    // Updating slot 2 syncs w3 slider but does NOT overwrite wPresetBtn preset commandValue
    canvas = store.getState().canvases[canvasId];
    assert.equal(canvas.widgets.find((w) => w.id === w3Id)?.sliderConfig?.value, -3);
    assert.equal(canvas.widgets.find((w) => w.id === wPresetBtnId)?.buttonConfig?.commandValue, 99);
    assert.deepEqual(store.getState().commands.find((c) => c.id === pidId)?.argValues, [
      1.5,
      0.25,
      -3,
    ]);

    // Persistence round-trip across commands, board, and project configs
    const cmdsJson = store.getState().exportCommandsConfig();
    const boardJson = store.getState().exportBoardConfig();
    const projectJson = store.getState().exportProjectConfig();

    store.getState().importCommandsConfig(cmdsJson, { replaceExisting: true });
    assert.deepEqual(store.getState().commands.find((c) => c.id === pidId)?.argValues, [
      1.5,
      0.25,
      -3,
    ]);

    await store.getState().importBoardConfig(boardJson);
    const boardRestoredCanvas = store.getState().canvases[canvasId];
    assert.equal(
      boardRestoredCanvas.widgets.find((w) => w.id === w1Id)?.sliderConfig?.autoSend,
      false,
    );
    assert.equal(
      boardRestoredCanvas.widgets.find((w) => w.id === w2Id)?.sliderConfig?.commandArgIndex,
      1,
    );
    assert.equal(
      boardRestoredCanvas.widgets.find((w) => w.id === wTrigAllId)?.buttonConfig?.commandArgIndex,
      -1,
    );
    assert.deepEqual(store.getState().commands.find((c) => c.id === pidId)?.argValues, [
      1.5,
      0.25,
      -3,
    ]);

    store.getState().removeCommand(pidId);
    await store.getState().importProjectConfig(projectJson);
    assert.deepEqual(store.getState().commands.find((c) => c.id === pidId)?.argValues, [
      1.5,
      0.25,
      -3,
    ]);
    assert.equal(
      store.getState().canvases[canvasId].widgets.find((w) => w.id === w3Id)?.sliderConfig?.commandArgIndex,
      2,
    );
  } finally {
    client.stopDataPlane();
  }
});

test("Command Table widget supports multi-row per-placeholder presets, independent row firing, source remap, unbind, and persistence", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const sentReqs: SendDownlinkRequestDto_Deserialize[] = [];
  const origSend = client.sendDownlink.bind(client);
  client.sendDownlink = async (req) => {
    sentReqs.push(req);
    return origSend(req);
  };

  const store = createWorkspaceStore(client);
  try {
    await store.getState().initWorkspace();

    const poseCmdId = store.getState().addCommand({
      name: "点位移动",
      format: "string",
      template: "pose %.1f %.1f %d",
      appendNewline: false,
      targetSourceId: "sim0",
      priority: "normal",
      argValues: [1.5, 2.5, 10],
    });
    const hexCmdId = store.getState().addCommand({
      name: "Hex包",
      format: "hex",
      template: "AA %hu %02X",
      endian: "big",
      targetSourceId: "sim0",
      priority: "high",
    });

    const canvasId = store.getState().activeCanvasId!;
    const tableWid = store.getState().addWidgetToCanvas(canvasId, "cmd_table", {
      col: 0,
      row: 12,
    });
    let tableWidget = store
      .getState()
      .canvases[canvasId].widgets.find((w) => w.id === tableWid);
    assert.equal(tableWidget?.type, "cmd_table");
    assert.equal(tableWidget?.cmdTableConfig?.commandId, null);
    assert.deepEqual(tableWidget?.cmdTableConfig?.rows, []);

    // Bind command -> automatically seeds 1 preset row from command argValues
    store.getState().bindCommandToWidget(canvasId, tableWid, poseCmdId);
    tableWidget = store
      .getState()
      .canvases[canvasId].widgets.find((w) => w.id === tableWid);
    assert.equal(tableWidget?.title, "点位移动");
    assert.equal(tableWidget?.cmdTableConfig?.commandId, poseCmdId);
    assert.equal(tableWidget?.cmdTableConfig?.rows.length, 1);
    assert.deepEqual(tableWidget?.cmdTableConfig?.rows[0].values, [1.5, 2.5, 10]);

    // Add Row 2 and Row 3 with distinct labels & per-placeholder column values
    const row1 = {
      ...tableWidget!.cmdTableConfig!.rows[0],
      label: "原点",
      values: updateCmdTableRowValue(
        updateCmdTableRowValue(
          updateCmdTableRowValue(tableWidget!.cmdTableConfig!.rows[0].values, 0, 0),
          1,
          0,
        ),
        2,
        100,
      ),
    };
    const row2Base = createCmdTableRow([row1], 3, [1.5, 2.5, 10], "zh-CN");
    const row2 = {
      ...row2Base,
      label: "工作点A",
      values: updateCmdTableRowValue(
        updateCmdTableRowValue(row2Base.values, 0, 12.5),
        2,
        300,
      ),
    };
    const row3Base = createCmdTableRow([row1, row2], 3, undefined, "en");
    const row3 = {
      ...row3Base,
      label: "返航点B",
      values: updateCmdTableRowValue(
        updateCmdTableRowValue(
          updateCmdTableRowValue(row3Base.values, 0, -4.2),
          1,
          8.8,
        ),
        2,
        50,
      ),
    };

    // Preserve extra columns even when updating column 0
    const preserved = updateCmdTableRowValue([10, 20, 30], 0, 99);
    assert.deepEqual(preserved, [99, 20, 30]);

    store.getState().updateCanvasWidget(canvasId, tableWid, (prev) => ({
      ...prev,
      cmdTableConfig: {
        ...prev.cmdTableConfig!,
        rows: [row1, row2, row3],
      },
    }));

    // Individual row compilation & execution (Row 1, Row 2, Row 3)
    const poseCmd = store.getState().commands.find((c) => c.id === poseCmdId)!;
    assert.equal(compileCmdTableRow(poseCmd, row1).compiled.preview, "pose 0.0 0.0 100");
    assert.equal(compileCmdTableRow(poseCmd, row2).compiled.preview, "pose 12.5 0.0 300");
    assert.equal(compileCmdTableRow(poseCmd, row3).compiled.preview, "pose -4.2 8.8 50");

    // Multi-placeholder Hex row compilation
    const hexCmd = store.getState().commands.find((c) => c.id === hexCmdId)!;
    const hexRowCompiled = compileCmdTableRow(hexCmd, { values: [513, 171] });
    assert.equal(hexRowCompiled.compiled.ok, true);
    if (hexRowCompiled.compiled.ok) {
      assert.deepEqual(hexRowCompiled.compiled.payload, {
        kind: "hex_bytes",
        hex: "AA 02 01 AB",
      });
    }

    await store.getState().executeCommand(poseCmdId, { values: row2.values });
    await store.getState().executeCommand(poseCmdId, { values: row1.values });
    await store.getState().executeCommand(poseCmdId, { values: row3.values });

    assert.equal(sentReqs.length, 3);
    assert.deepEqual(sentReqs[0].payload, {
      kind: "ascii_text",
      text: "pose 12.5 0.0 300",
      append_newline: false,
    });
    assert.deepEqual(sentReqs[1].payload, {
      kind: "ascii_text",
      text: "pose 0.0 0.0 100",
      append_newline: false,
    });
    assert.deepEqual(sentReqs[2].payload, {
      kind: "ascii_text",
      text: "pose -4.2 8.8 50",
      append_newline: false,
    });

    // Persistence round-trip across Board and Project configs
    const boardJson = store.getState().exportBoardConfig();
    const projectJson = store.getState().exportProjectConfig();

    store.getState().removeCanvasWidget(canvasId, tableWid);
    await store.getState().importBoardConfig(boardJson);
    const boardRestored = store
      .getState()
      .canvases[canvasId].widgets.find((w) => w.id === tableWid);
    assert.equal(boardRestored?.type, "cmd_table");
    assert.equal(boardRestored?.cmdTableConfig?.commandId, poseCmdId);
    assert.equal(boardRestored?.cmdTableConfig?.rows.length, 3);
    assert.equal(boardRestored?.cmdTableConfig?.rows[1].label, "工作点A");
    assert.deepEqual(boardRestored?.cmdTableConfig?.rows[2].values, [-4.2, 8.8, 50]);

    store.getState().removeCommand(poseCmdId);
    await store.getState().importProjectConfig(projectJson);
    const projectRestored = store
      .getState()
      .canvases[canvasId].widgets.find((w) => w.id === tableWid);
    assert.equal(projectRestored?.cmdTableConfig?.commandId, poseCmdId);
    assert.deepEqual(projectRestored?.cmdTableConfig?.rows[0].values, [0, 0, 100]);

    // Source rename remaps cmdTableConfig.sourceId
    await store.getState().createSource({
      source_id: "rig_a",
      display_name: "Rig A",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9115",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: "127.0.0.1:9116",
      },
      clock: { mode: "tier2_software_pll", nominal_sample_rate_hz: 50_000 },
      protocol: { codec: "just_float", channel_names: ["va", "vb"] },
      auto_start: true,
    });
    store.getState().updateCanvasWidget(canvasId, tableWid, (prev) => ({
      ...prev,
      cmdTableConfig: { ...prev.cmdTableConfig!, sourceId: "rig_a" },
    }));
    await store.getState().updateSource("rig_a", {
      source_id: "rig_b",
      display_name: "Rig B",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9115",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: "127.0.0.1:9210",
      },
      clock: { mode: "tier2_software_pll", nominal_sample_rate_hz: 50_000 },
      protocol: { codec: "just_float", channel_names: ["va", "vb"] },
      auto_start: true,
    });
    assert.equal(
      store.getState().canvases[canvasId].widgets.find((w) => w.id === tableWid)
        ?.cmdTableConfig?.sourceId,
      "rig_b",
    );

    // Deleting bound command unbinds cmdTableConfig.commandId while keeping rows
    store.getState().removeCommand(poseCmdId);
    const afterRemove = store
      .getState()
      .canvases[canvasId].widgets.find((w) => w.id === tableWid);
    assert.equal(afterRemove?.cmdTableConfig?.commandId, null);
    assert.equal(afterRemove?.cmdTableConfig?.rows.length, 3);
  } finally {
    client.stopDataPlane();
  }
});
