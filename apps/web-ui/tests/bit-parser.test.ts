import test from "node:test";
import assert from "node:assert/strict";
import { BridgeClient, HighFreqTopicBus } from "../src/bridge/index.ts";
import {
  createDefaultBitParserConfig,
  evaluateActiveBits,
  formatBitWordHex,
  isBitSet,
  normalizeBitParserConfig,
  toUnsignedBitWord,
  upsertBitConfigItem,
} from "../src/widgets/bit-parser.ts";
import { WIDGET_CATALOG, WIDGET_TYPES } from "../src/widgets/types.ts";
import {
  parseBoardConfigJson,
  parseProjectConfigJson,
} from "../src/workspace/config-io.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("toUnsignedBitWord, isBitSet, and formatBitWordHex decode 8/16/32-bit values accurately", () => {
  assert.ok(WIDGET_TYPES.includes("bit_parser"));
  assert.ok(WIDGET_CATALOG.some((entry) => entry.type === "bit_parser" && entry.badge === "BIT"));

  // Non-finite inputs must yield null
  assert.equal(toUnsignedBitWord(Number.NaN, 16), null);
  assert.equal(toUnsignedBitWord(Number.POSITIVE_INFINITY, 32), null);
  assert.equal(toUnsignedBitWord(Number.NEGATIVE_INFINITY, 8), null);

  // 8-bit masking & formatting
  assert.equal(toUnsignedBitWord(0, 8), 0);
  assert.equal(toUnsignedBitWord(0x1ff, 8), 0xff);
  assert.equal(toUnsignedBitWord(-1, 8), 0xff);
  assert.equal(formatBitWordHex(0x0a, 8), "0x0A");
  assert.equal(formatBitWordHex(0xff, 8), "0xFF");

  // 16-bit rounding, signed wrap, and formatting
  assert.equal(toUnsignedBitWord(5.4, 16), 5);
  assert.equal(toUnsignedBitWord(5.6, 16), 6);
  assert.equal(toUnsignedBitWord(65537, 16), 1);
  assert.equal(toUnsignedBitWord(-2, 16), 0xfffe);
  assert.equal(formatBitWordHex(0x0105, 16), "0x0105");
  assert.equal(formatBitWordHex(0xfffe, 16), "0xFFFE");

  // 32-bit MSB bit 31 without signed shift bug
  const msb32 = toUnsignedBitWord(0x80000001, 32)!;
  assert.equal(msb32, 0x80000001);
  assert.equal(isBitSet(msb32, 0), true);
  assert.equal(isBitSet(msb32, 30), false);
  assert.equal(isBitSet(msb32, 31), true);
  assert.equal(formatBitWordHex(msb32, 32), "0x80000001");
  assert.equal(toUnsignedBitWord(-1, 32), 0xffffffff);
  assert.equal(formatBitWordHex(0xffffffff, 32), "0xFFFFFFFF");

  // Invalid bit indices must safely return false
  assert.equal(isBitSet(0xffffffff, -1), false);
  assert.equal(isBitSet(0xffffffff, 32), false);
  assert.equal(isBitSet(0xffffffff, 1.5), false);
});

test("evaluateActiveBits shows messages only for bit=1 and honors bitWidth and showUnlabeled", () => {
  // Fresh config with 0 labels falls back to Bit N for active bits
  const freshCfg = createDefaultBitParserConfig();
  assert.deepEqual(evaluateActiveBits(0b101, freshCfg), [
    { bit: 0, label: "Bit 0", tone: "info", isFallback: true },
    { bit: 2, label: "Bit 2", tone: "info", isFallback: true },
  ]);
  assert.deepEqual(evaluateActiveBits(0, freshCfg), []);

  // Configure bit 3, bit 0, bit 10 out of order and verify sorting
  let cfg = upsertBitConfigItem(freshCfg, 3, { label: "Over Temp", tone: "warn" });
  cfg = upsertBitConfigItem(cfg, 0, { label: "Over Voltage", tone: "fault" });
  cfg = upsertBitConfigItem(cfg, 10, { label: "Ready", tone: "ok" });
  assert.deepEqual(
    cfg.bits.map((b) => b.bit),
    [0, 3, 10],
  );

  // Guard invalid bit indices
  assert.equal(upsertBitConfigItem(cfg, -1, { label: "Bad" }), cfg);
  assert.equal(upsertBitConfigItem(cfg, 32, { label: "Bad" }), cfg);

  // Only bits with bit=1 are emitted; unlabeled set bit 1 is hidden when showUnlabeled=false
  assert.deepEqual(evaluateActiveBits((1 << 0) | (1 << 1) | (1 << 10), cfg), [
    { bit: 0, label: "Over Voltage", tone: "fault", isFallback: false },
    { bit: 10, label: "Ready", tone: "ok", isFallback: false },
  ]);

  // Bit 0 clears -> Over Voltage disappears
  assert.deepEqual(evaluateActiveBits(1 << 3, cfg), [
    { bit: 3, label: "Over Temp", tone: "warn", isFallback: false },
  ]);

  // showUnlabeled=true emits unlabeled set bits as fallback alongside configured bits
  const withFallbackCfg = { ...cfg, showUnlabeled: true };
  assert.deepEqual(evaluateActiveBits((1 << 1) | (1 << 3), withFallbackCfg), [
    { bit: 1, label: "Bit 1", tone: "info", isFallback: true },
    { bit: 3, label: "Over Temp", tone: "warn", isFallback: false },
  ]);

  // Bits >= bitWidth are ignored
  const width8Cfg = { ...cfg, bitWidth: 8 as const };
  assert.deepEqual(evaluateActiveBits((1 << 0) | (1 << 10), width8Cfg), [
    { bit: 0, label: "Over Voltage", tone: "fault", isFallback: false },
  ]);

  // Clearing both label and tone deletes the bit config item
  const trimmed = upsertBitConfigItem(cfg, 3, { label: "", tone: "info" });
  assert.deepEqual(
    trimmed.bits.map((b) => b.bit),
    [0, 10],
  );

  // normalizeBitParserConfig sanitizes untrusted inputs
  assert.equal(normalizeBitParserConfig("invalid"), undefined);
  assert.deepEqual(
    normalizeBitParserConfig({
      bitWidth: 24,
      showUnlabeled: 1,
      bits: [
        { bit: 5, label: "Motor Run", tone: "ok" },
        { bit: 5, label: "Duplicate", tone: "fault" },
        { bit: 2, label: "   ", tone: "warn" },
        { bit: 99, label: "Out of range", tone: "info" },
        { bit: 1, label: "Door Open", tone: "unknown" },
      ],
    }),
    {
      bitWidth: 16,
      showUnlabeled: true,
      bits: [
        { bit: 1, label: "Door Open", tone: "info" },
        { bit: 5, label: "Motor Run", tone: "ok" },
      ],
    },
  );
});

test("WorkspaceStore and config-io persist and rebind bit_parser widgets cleanly", async () => {
  const bus = new HighFreqTopicBus();
  const client = new BridgeClient(bus);
  const store = createWorkspaceStore(client);

  try {
    await store.getState().initWorkspace();
    store.getState().setLocale("zh-CN");

    await store.getState().createSource({
      source_id: "udp_bits",
      display_name: "Status Source",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9201",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: null,
      },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 1000,
      },
      protocol: {
        codec: "just_float",
        channel_names: ["status_word", "aux_word"],
      },
      auto_start: false,
    });

    const channels = Object.values(store.getState().channelsById);
    const statusCh = channels.find((c) => c.full_path === "udp_bits/status_word");
    const auxCh = channels.find((c) => c.full_path === "udp_bits/aux_word");
    assert.ok(statusCh && auxCh);

    const cid = store.getState().activeCanvasId!;
    const wid = store.getState().addWidgetToCanvas(cid, "bit_parser");
    const initialWidget = store.getState().canvases[cid].widgets.find((w) => w.id === wid)!;
    assert.equal(initialWidget.type, "bit_parser");
    assert.equal(initialWidget.title, "Bit 解析器");
    assert.deepEqual(initialWidget.bitParserConfig, {
      bitWidth: 16,
      bits: [],
      showUnlabeled: false,
    });

    // Binding first channel replaces default toolbox title with channel full_path
    store.getState().bindChannelToWidget(cid, wid, statusCh.channel_id);
    let widget = store.getState().canvases[cid].widgets.find((w) => w.id === wid)!;
    assert.equal(widget.primaryBinding?.channelId, statusCh.channel_id);
    assert.equal(widget.title, "udp_bits/status_word");

    // Configure custom bits, width, and custom widget title
    store.getState().updateCanvasWidget(cid, wid, (prev) => ({
      ...prev,
      title: "Fault Decoder",
      bitParserConfig: {
        bitWidth: 32,
        showUnlabeled: true,
        bits: [
          { bit: 0, label: "Overcurrent", tone: "fault" },
          { bit: 31, label: "Heartbeat OK", tone: "ok" },
        ],
      },
    }));

    // Rebinding channel preserves user's custom title & bitParserConfig
    store.getState().bindChannelToWidget(cid, wid, auxCh.channel_id);
    widget = store.getState().canvases[cid].widgets.find((w) => w.id === wid)!;
    assert.equal(widget.primaryBinding?.channelId, auxCh.channel_id);
    assert.equal(widget.title, "Fault Decoder");
    assert.equal(widget.bitParserConfig?.bitWidth, 32);

    // Bind back to statusCh and edit source to confirm rebinding survival
    store.getState().bindChannelToWidget(cid, wid, statusCh.channel_id);
    await store.getState().updateSource("udp_bits", {
      source_id: "udp_bits",
      display_name: "Status Source Updated",
      transport: {
        kind: "udp",
        bind_addr: "0.0.0.0:9202",
        multicast_group: null,
        recv_buffer_bytes: 1_048_576,
        remote_tx_addr: null,
      },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 2000,
      },
      protocol: {
        codec: "just_float",
        channel_names: ["status_word", "aux_word"],
      },
      auto_start: false,
    });

    const reboundStatus = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "udp_bits/status_word",
    );
    assert.ok(reboundStatus);
    widget = store.getState().canvases[cid].widgets.find((w) => w.id === wid)!;
    assert.equal(widget.primaryBinding?.channelId, reboundStatus.channel_id);
    assert.deepEqual(widget.bitParserConfig?.bits, [
      { bit: 0, label: "Overcurrent", tone: "fault" },
      { bit: 31, label: "Heartbeat OK", tone: "ok" },
    ]);

    // Export board + project JSON and rehydrate cleanly
    const boardJson = store.getState().exportBoardConfig();
    const projectJson = store.getState().exportProjectConfig();
    const parsedBoard = parseBoardConfigJson(boardJson);
    const parsedProject = parseProjectConfigJson(projectJson);

    const exportedBoardWidget = parsedBoard.canvases[0].widgets.find((w) => w.id === wid)!;
    assert.equal(exportedBoardWidget.bitParserConfig?.bitWidth, 32);
    assert.equal(exportedBoardWidget.bitParserConfig?.showUnlabeled, true);
    assert.equal(parsedProject.board.canvases[0].widgets.find((w) => w.id === wid)?.title, "Fault Decoder");

    // Clear canvas and verify importBoardConfig restores bit_parser
    store.getState().clearCanvasWidgets(cid);
    assert.equal(store.getState().canvases[cid].widgets.length, 0);

    await store.getState().importBoardConfig(boardJson);
    const restoredBoardWidget = store.getState().canvases[cid].widgets.find((w) => w.id === wid)!;
    assert.equal(restoredBoardWidget.type, "bit_parser");
    assert.equal(restoredBoardWidget.title, "Fault Decoder");
    assert.equal(restoredBoardWidget.primaryBinding?.channelId, reboundStatus.channel_id);
    assert.deepEqual(restoredBoardWidget.bitParserConfig, {
      bitWidth: 32,
      showUnlabeled: true,
      bits: [
        { bit: 0, label: "Overcurrent", tone: "fault" },
        { bit: 31, label: "Heartbeat OK", tone: "ok" },
      ],
    });

    // Clear and restore via importProjectConfig
    store.getState().clearCanvasWidgets(cid);
    await store.getState().importProjectConfig(projectJson);
    const restoredProjCh = Object.values(store.getState().channelsById).find(
      (c) => c.full_path === "udp_bits/status_word",
    );
    assert.ok(restoredProjCh);
    const restoredProjWidget = store.getState().canvases[cid].widgets.find((w) => w.id === wid)!;
    assert.equal(restoredProjWidget.type, "bit_parser");
    assert.equal(restoredProjWidget.primaryBinding?.channelId, restoredProjCh.channel_id);
    assert.deepEqual(restoredProjWidget.bitParserConfig, {
      bitWidth: 32,
      showUnlabeled: true,
      bits: [
        { bit: 0, label: "Overcurrent", tone: "fault" },
        { bit: 31, label: "Heartbeat OK", tone: "ok" },
      ],
    });
  } finally {
    client.stopDataPlane();
  }
});
