import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;
const configDir = path.dirname(fileURLToPath(import.meta.url));
const repoPluginsDir = path.resolve(configDir, "../../plugins");

function resolvePluginsDir(): string {
  const envDir = process.env.DDSCOPE_PLUGINS_DIR;
  if (envDir && envDir.trim().length > 0) {
    return path.resolve(envDir.split(path.delimiter)[0]);
  }
  return repoPluginsDir;
}

function readLocalizedText(val: unknown): { en: string; zh: string } {
  if (typeof val === "string") return { en: val, zh: val };
  if (val && typeof val === "object") {
    const rec = val as Record<string, unknown>;
    const en = typeof rec.en === "string" ? rec.en : "";
    const zh = typeof rec["zh-CN"] === "string" ? rec["zh-CN"] : en;
    return { en, zh };
  }
  return { en: "", zh: "" };
}

function mapParamSpecs(rawParams: unknown) {
  if (!Array.isArray(rawParams)) return [];
  return rawParams
    .filter((p): p is Record<string, unknown> => Boolean(p && typeof p === "object"))
    .map((p) => {
      const label = readLocalizedText(p.label);
      const desc = p.description !== undefined ? readLocalizedText(p.description) : null;
      return {
        key: String(p.key ?? ""),
        label_en: label.en || String(p.key ?? ""),
        label_zh: label.zh || label.en || String(p.key ?? ""),
        param_type: typeof p.type === "string" ? p.type : "string",
        default_json: JSON.stringify(p.default ?? null),
        min: typeof p.min === "number" ? p.min : null,
        max: typeof p.max === "number" ? p.max : null,
        step: typeof p.step === "number" ? p.step : null,
        options: Array.isArray(p.options)
          ? p.options
              .filter((o): o is Record<string, unknown> => Boolean(o && typeof o === "object"))
              .map((o) => {
                const ol = readLocalizedText(o.label);
                return {
                  value: String(o.value ?? ""),
                  label_en: ol.en || String(o.value ?? ""),
                  label_zh: ol.zh || ol.en || String(o.value ?? ""),
                };
              })
          : [],
        description_en: desc ? desc.en || null : null,
        description_zh: desc ? desc.zh || null : null,
      };
    });
}

function scanExternalPluginsCatalog() {
  const rootDir = resolvePluginsDir();
  const protocols: unknown[] = [];
  const widgets: unknown[] = [];

  const protoDir = path.join(rootDir, "protocols");
  if (fs.existsSync(protoDir) && fs.statSync(protoDir).isDirectory()) {
    for (const dirent of fs.readdirSync(protoDir, { withFileTypes: true })) {
      if (!dirent.isDirectory()) continue;
      const manifestPath = path.join(protoDir, dirent.name, "plugin.json");
      if (!fs.existsSync(manifestPath)) continue;
      try {
        const m = JSON.parse(fs.readFileSync(manifestPath, "utf8")) as Record<string, unknown>;
        const name = readLocalizedText(m.name);
        const desc = m.description !== undefined ? readLocalizedText(m.description) : null;
        const backend = (m.backend as Record<string, unknown> | undefined) ?? {};
        protocols.push({
          id: String(m.id ?? dirent.name),
          name_en: name.en || String(m.id ?? dirent.name),
          name_zh: name.zh || name.en || String(m.id ?? dirent.name),
          description_en: desc ? desc.en || null : null,
          description_zh: desc ? desc.zh || null : null,
          version: String(m.version ?? "1.0.0"),
          backend_mode: String(backend.mode ?? "declarative"),
          available: m.schema === "ddscope.protocol-plugin.v1",
          error:
            m.schema === "ddscope.protocol-plugin.v1"
              ? null
              : `Unsupported schema '${String(m.schema ?? "")}'`,
          manifest_path: manifestPath,
          params: mapParamSpecs(m.params),
          channels_preview: Array.isArray(m.channels_preview)
            ? m.channels_preview.map((c: Record<string, unknown>) => ({
                name: String(c.name ?? ""),
                scalar_type: String(c.scalar_type ?? "f32"),
                unit: typeof c.unit === "string" ? c.unit : null,
              }))
            : [],
        });
      } catch {
        // Skip malformed plugin.json in dev middleware
      }
    }
  }

  const widgetDir = path.join(rootDir, "widgets");
  if (fs.existsSync(widgetDir) && fs.statSync(widgetDir).isDirectory()) {
    for (const dirent of fs.readdirSync(widgetDir, { withFileTypes: true })) {
      if (!dirent.isDirectory()) continue;
      const pluginDir = path.join(widgetDir, dirent.name);
      const manifestPath = path.join(pluginDir, "plugin.json");
      if (!fs.existsSync(manifestPath)) continue;
      try {
        const m = JSON.parse(fs.readFileSync(manifestPath, "utf8")) as Record<string, unknown>;
        const name = readLocalizedText(m.name);
        const desc = m.description !== undefined ? readLocalizedText(m.description) : null;
        const entryJs = typeof m.entry_js === "string" ? m.entry_js : "index.js";
        const styleCss = typeof m.style_css === "string" ? m.style_css : null;
        const entryOk = fs.existsSync(path.join(pluginDir, entryJs));
        const styleOk = !styleCss || fs.existsSync(path.join(pluginDir, styleCss));
        widgets.push({
          id: String(m.id ?? dirent.name),
          name_en: name.en || String(m.id ?? dirent.name),
          name_zh: name.zh || name.en || String(m.id ?? dirent.name),
          description_en: desc ? desc.en || null : null,
          description_zh: desc ? desc.zh || null : null,
          version: String(m.version ?? "1.0.0"),
          badge: typeof m.badge === "string" && m.badge.trim() ? m.badge : "PLG",
          default_col_span: typeof m.default_col_span === "number" ? m.default_col_span : 8,
          default_row_span: typeof m.default_row_span === "number" ? m.default_row_span : 4,
          binding_mode: typeof m.binding_mode === "string" ? m.binding_mode : "single_channel",
          available: m.schema === "ddscope.widget-plugin.v1" && entryOk && styleOk,
          error: !entryOk ? `Missing ${entryJs}` : !styleOk ? `Missing ${styleCss}` : null,
          manifest_path: manifestPath,
          params: mapParamSpecs(m.params),
        });
      } catch {
        // Skip malformed plugin.json in dev middleware
      }
    }
  }

  return {
    search_dirs: fs.existsSync(rootDir) ? [rootDir] : [],
    protocols,
    widgets,
  };
}

function readDevWidgetBundle(pluginId: string): {
  id: string;
  entry_js: string;
  style_css: string | null;
} | null {
  if (!/^[a-z0-9_-]{1,64}$/.test(pluginId)) return null;
  const widgetRoot = path.join(resolvePluginsDir(), "widgets");
  if (!fs.existsSync(widgetRoot)) return null;
  for (const dirent of fs.readdirSync(widgetRoot, { withFileTypes: true })) {
    if (!dirent.isDirectory()) continue;
    const pluginDir = path.join(widgetRoot, dirent.name);
    const manifestPath = path.join(pluginDir, "plugin.json");
    if (!fs.existsSync(manifestPath)) continue;
    const m = JSON.parse(fs.readFileSync(manifestPath, "utf8")) as Record<string, unknown>;
    if (m.id !== pluginId) continue;
    const entryRel = typeof m.entry_js === "string" ? m.entry_js : "index.js";
    const styleRel = typeof m.style_css === "string" ? m.style_css : null;
    const dirCanon = fs.realpathSync(pluginDir);
    const entryAbs = fs.realpathSync(path.resolve(pluginDir, entryRel));
    if (!entryAbs.startsWith(dirCanon + path.sep)) return null;
    const entry_js = fs.readFileSync(entryAbs, "utf8");
    let style_css: string | null = null;
    if (styleRel) {
      const styleAbs = fs.realpathSync(path.resolve(pluginDir, styleRel));
      if (!styleAbs.startsWith(dirCanon + path.sep)) return null;
      style_css = fs.readFileSync(styleAbs, "utf8");
    }
    return { id: pluginId, entry_js, style_css };
  }
  return null;
}

function ddscopeExternalPluginsDevPlugin(): Plugin {
  return {
    name: "ddscope-external-plugins-dev",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const urlPath = (req.url ?? "").split("?")[0];
        if (
          urlPath === "/api/ddscope-plugins/catalog" ||
          urlPath === "/api/ddscope-plugins/reload"
        ) {
          const payload = scanExternalPluginsCatalog();
          res.statusCode = 200;
          res.setHeader("Content-Type", "application/json; charset=utf-8");
          res.end(JSON.stringify(payload));
          return;
        }
        const widgetPrefix = "/api/ddscope-plugins/widget/";
        if (urlPath.startsWith(widgetPrefix)) {
          const pluginId = decodeURIComponent(urlPath.slice(widgetPrefix.length));
          const bundle = readDevWidgetBundle(pluginId);
          if (!bundle) {
            res.statusCode = 404;
            res.setHeader("Content-Type", "application/json; charset=utf-8");
            res.end(JSON.stringify({ error: `Widget plugin '${pluginId}' not found` }));
            return;
          }
          res.statusCode = 200;
          res.setHeader("Content-Type", "application/json; charset=utf-8");
          res.end(JSON.stringify(bundle));
          return;
        }
        next();
      });
    },
  };
}

export default defineConfig({
  plugins: [react(), ddscopeExternalPluginsDevPlugin()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/apps/desktop/**", "**/crates/**", "**/target/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          const norm = id.replaceAll("\\", "/");
          if (!norm.includes("/node_modules/")) return undefined;
          if (/\/node_modules\/(dockview|dockview-react|dockview-core)\//.test(norm)) {
            return "vendor-dockview";
          }
          if (/\/node_modules\/(react|react-dom|scheduler|zustand)\//.test(norm)) {
            return "vendor-react";
          }
          return undefined;
        },
      },
    },
  },
});
