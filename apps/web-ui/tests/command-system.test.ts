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
  commandUsesValue,
  compileCommandPayload,
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
