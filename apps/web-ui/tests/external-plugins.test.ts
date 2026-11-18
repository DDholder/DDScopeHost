import test from "node:test";
import assert from "node:assert/strict";
import {
  BridgeClient,
  HighFreqTopicBus,
  normalizeSourceConfigToSerialize,
} from "../src/bridge/index.ts";
import { MockSimulationCore } from "../src/bridge/mock-ddsf-core.ts";
import {
  buildDefaultPluginParams,
  isPluginWidgetType,
  resetExternalPluginCatalogForTests,
  setExternalPluginCatalog,
  toPluginWidgetType,
} from "../src/widgets/plugin-registry.ts";
import { defaultSizeForWidgetType, isValidWidgetType } from "../src/widgets/types.ts";
import {
  BOARD_CONFIG_SCHEMA_V1,
  PROJECT_CONFIG_SCHEMA_V1,
  SOURCES_CONFIG_SCHEMA_V1,
  parseBoardConfigJson,
  parseProjectConfigJson,
  parseSourcesConfigJson,
  serializeSourcesConfig,
} from "../src/workspace/config-io.ts";
import { createWorkspaceStore } from "../src/workspace/store.ts";

test("external widget plugin types validate, populate defaults, and survive board/project roundtrips even when unloaded", async () => {
  resetExternalPluginCatalogForTests();
  try {
    setExternalPluginCatalog({
      search_dirs: ["./plugins"],
      protocols: [],
      widgets: [
        {
          id: "bar_level_monitor",
          name_en: "Bar Level Monitor",
          name_zh: "柱状电平监视器",
          description_en: "Bar level",
          description_zh: "柱状电平",
          version: "1.0.0",
          badge: "BAR",
          default_col_span: 10,
          default_row_span: 5,
          binding_mode: "single_channel",
          available: true,
          error: null,
          manifest_path: "./plugins/widgets/bar_level_monitor/plugin.json",
          params: [
            {
              key: "min",
              label_en: "Min",
              label_zh: "下限",
              param_type: "number",
              default_json: "-25",
              min: null,
              max: null,
              step: 1,
              options: [],
              description_en: null,
              description_zh: null,
            },
            {
              key: "warnThreshold",
              label_en: "Warn",
              label_zh: "告警",
              param_type: "number",
              default_json: "18",
              min: null,
              max: null,
              step: 0.5,
              options: [],
              description_en: null,
              description_zh: null,
            },
            {
              key: "showPeakHold",
              label_en: "Peak Hold",
              label_zh: "峰值保持",
              param_type: "boolean",
              default_json: "true",
              min: null,
              max: null,
              step: null,
              options: [],
              description_en: null,
              description_zh: null,
            },
          ],
        },
      ],
    });

    const widgetType = toPluginWidgetType("bar_level_monitor");
    assert.equal(isPluginWidgetType(widgetType), true);
    assert.equal(isValidWidgetType(widgetType), true);
    assert.equal(isValidWidgetType("plugin:../evil"), false);
    assert.deepEqual(defaultSizeForWidgetType(widgetType), { colSpan: 10, rowSpan: 5 });

    const defaults = buildDefaultPluginParams([]);
    assert.deepEqual(defaults, {});

    const client = new BridgeClient(new HighFreqTopicBus());
    const store = createWorkspaceStore(client);
    try {
      await store.getState().initWorkspace();

      const canvasId = store.getState().activeCanvasId ?? store.getState().canvasOrder[0];
      const ia = Object.values(store.getState().channelsById).find(
        (c) => c.local_path === "ia" || c.local_path === "ch0",
      );
      assert.ok(ia);

      const wId = store.getState().addWidgetToCanvas(canvasId, widgetType, {
        channelId: ia.channel_id,
      });
      const added = store
        .getState()
        .canvases[canvasId].widgets.find((w) => w.id === wId);
      assert.ok(added);
      assert.equal(added.colSpan, 10);
      assert.equal(added.rowSpan, 5);
      assert.deepEqual(added.pluginConfig, {
        min: -25,
        warnThreshold: 18,
        showPeakHold: true,
      });

      store.getState().updateCanvasWidget(canvasId, wId, (prev) => ({
        ...prev,
        pluginConfig: {
          ...(prev.pluginConfig ?? {}),
          warnThreshold: 22.5,
        },
      }));

      const boardJson = store.getState().exportBoardConfig();
      const projectJson = store.getState().exportProjectConfig();

      // Unload all plugins and re-import — widget config & type must still be preserved cleanly
      resetExternalPluginCatalogForTests();
      const parsedBoard = parseBoardConfigJson(boardJson);
      assert.equal(parsedBoard.schema, BOARD_CONFIG_SCHEMA_V1);
      const parsedProject = parseProjectConfigJson(projectJson);
      assert.equal(parsedProject.schema, PROJECT_CONFIG_SCHEMA_V1);

      await store.getState().importBoardConfig(boardJson);
      const restored = store
        .getState()
        .canvases[canvasId].widgets.find((w) => w.id === wId);
      assert.ok(restored);
      assert.equal(restored.type, "plugin:bar_level_monitor");
      assert.deepEqual(restored.pluginConfig, {
        min: -25,
        warnThreshold: 22.5,
        showPeakHold: true,
      });
    } finally {
      client.stopDataPlane();
    }
  } finally {
    resetExternalPluginCatalogForTests();
  }
});

test("external protocol plugins normalize, round-trip sources/projects, and register channels in MockSimulationCore", async () => {
  resetExternalPluginCatalogForTests();
  try {
    setExternalPluginCatalog({
      search_dirs: ["./plugins"],
      protocols: [
        {
          id: "custom_xor_frame",
          name_en: "XOR Frame",
          name_zh: "XOR 帧",
          description_en: null,
          description_zh: null,
          version: "1.0.0",
          backend_mode: "cdylib",
          available: true,
          error: null,
          manifest_path: "./plugins/protocols/custom_xor_frame/plugin.json",
          params: [],
          channels_preview: [
            { name: "xor_ia", scalar_type: "f32", unit: "A" },
            { name: "xor_ib", scalar_type: "f32", unit: "A" },
            { name: "xor_seq", scalar_type: "u32", unit: null },
          ],
        },
      ],
      widgets: [],
    });

    const normalized = normalizeSourceConfigToSerialize({
      source_id: "xor_mcu",
      display_name: "XOR MCU",
      transport: {
        kind: "serial",
        port_name: "COM9",
        baud_rate: 921_600,
        data_bits: 8,
        parity: "none",
        stop_bits: "one",
        flow_control: "none",
        auto_reconnect: true,
      },
      clock: {
        mode: "tier2_software_pll",
        nominal_sample_rate_hz: 100_000,
      },
      protocol: {
        codec: "plugin",
        plugin_id: " custom_xor_frame ",
        config_json: '{"xor_key":60}',
      },
      auto_start: false,
    });

    assert.deepEqual(normalized.protocol, {
      codec: "plugin",
      plugin_id: "custom_xor_frame",
      config_json: '{"xor_key":60}',
    });

    const sourcesJson = serializeSourcesConfig([normalized]);
    const parsedSources = parseSourcesConfigJson(sourcesJson);
    assert.equal(parsedSources.schema, SOURCES_CONFIG_SCHEMA_V1);
    assert.deepEqual(parsedSources.sources[0].protocol, normalized.protocol);

    const mock = new MockSimulationCore();
    mock.createSource(normalized);
    const tree = mock.getChannelTree();
    const grp = tree.sources.find((s) => s.source.source_id === "xor_mcu");
    assert.ok(grp);
    assert.deepEqual(
      grp.channels.map((c) => c.local_path),
      ["xor_ia", "xor_ib", "xor_seq"],
    );
  } finally {
    resetExternalPluginCatalogForTests();
  }
});
