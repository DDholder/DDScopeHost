import type {
  ExternalPluginCatalogDto_Serialize,
  PluginParamSpecDto_Deserialize,
  ProtocolPluginInfoDto_Serialize,
  WidgetPluginBundleDto_Serialize,
  WidgetPluginInfoDto_Serialize,
} from "../bridge/bindings.ts";
import type { UiLocale } from "../i18n/index.ts";

export const PLUGIN_WIDGET_PREFIX = "plugin:" as const;
export type PluginWidgetType = `${typeof PLUGIN_WIDGET_PREFIX}${string}`;

export interface MountedWidgetPluginInstance {
  update?: (state: Record<string, unknown>) => void;
  unmount?: () => void;
}

export interface ExternalWidgetPluginModule {
  mount: (
    container: HTMLElement,
    api: {
      sendDownlink?: (payload: unknown) => void;
      updateConfig?: (patch: Record<string, unknown>) => void;
    },
  ) => MountedWidgetPluginInstance | void;
}

const VALID_PLUGIN_ID_RE = /^[a-z0-9_-]{1,64}$/;

export function isValidExternalPluginId(value: unknown): value is string {
  return typeof value === "string" && VALID_PLUGIN_ID_RE.test(value);
}

export function isPluginWidgetType(value: unknown): value is PluginWidgetType {
  if (typeof value !== "string" || !value.startsWith(PLUGIN_WIDGET_PREFIX)) {
    return false;
  }
  return isValidExternalPluginId(value.slice(PLUGIN_WIDGET_PREFIX.length));
}

export function pluginIdFromWidgetType(type: PluginWidgetType): string {
  return type.slice(PLUGIN_WIDGET_PREFIX.length);
}

export function toPluginWidgetType(pluginId: string): PluginWidgetType {
  return `${PLUGIN_WIDGET_PREFIX}${pluginId}`;
}

export function resolvePluginText(
  en: string | null | undefined,
  zh: string | null | undefined,
  locale: UiLocale,
): string {
  if (locale === "zh-CN" && zh && zh.trim().length > 0) {
    return zh;
  }
  return en && en.trim().length > 0 ? en : zh ?? "";
}

const EMPTY_CATALOG: ExternalPluginCatalogDto_Serialize = {
  search_dirs: [],
  protocols: [],
  widgets: [],
};

let currentCatalog: ExternalPluginCatalogDto_Serialize = EMPTY_CATALOG;
const catalogListeners = new Set<() => void>();
const loadedModuleCache = new Map<string, Promise<ExternalWidgetPluginModule>>();

export function getExternalPluginCatalog(): ExternalPluginCatalogDto_Serialize {
  return currentCatalog;
}

export function setExternalPluginCatalog(catalog: ExternalPluginCatalogDto_Serialize): void {
  currentCatalog = {
    search_dirs: [...catalog.search_dirs],
    protocols: catalog.protocols.map((p) => ({
      ...p,
      params: p.params.map((pm) => ({ ...pm, options: pm.options.map((o) => ({ ...o })) })),
      channels_preview: p.channels_preview.map((c) => ({ ...c })),
    })),
    widgets: catalog.widgets.map((w) => ({
      ...w,
      params: w.params.map((pm) => ({ ...pm, options: pm.options.map((o) => ({ ...o })) })),
    })),
  };
  for (const cb of catalogListeners) {
    cb();
  }
}

export function resetExternalPluginCatalogForTests(): void {
  clearLoadedWidgetModuleCache();
  setExternalPluginCatalog(EMPTY_CATALOG);
}

export function subscribeExternalPluginCatalog(listener: () => void): () => void {
  catalogListeners.add(listener);
  return () => {
    catalogListeners.delete(listener);
  };
}

export function clearLoadedWidgetModuleCache(): void {
  loadedModuleCache.clear();
}

export function getPluginWidgetInfo(
  typeOrId: string,
): WidgetPluginInfoDto_Serialize | undefined {
  const id = isPluginWidgetType(typeOrId) ? pluginIdFromWidgetType(typeOrId) : typeOrId;
  return currentCatalog.widgets.find((w) => w.id === id);
}

export function getPluginProtocolInfo(
  pluginId: string,
): ProtocolPluginInfoDto_Serialize | undefined {
  return currentCatalog.protocols.find((p) => p.id === pluginId);
}

export function parsePluginParamDefault(defaultJson: string): unknown {
  if (!defaultJson || defaultJson.trim().length === 0) return undefined;
  try {
    return JSON.parse(defaultJson);
  } catch {
    return defaultJson;
  }
}

export function buildDefaultPluginParams(
  params: ReadonlyArray<PluginParamSpecDto_Deserialize>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const p of params) {
    const val = parsePluginParamDefault(p.default_json);
    if (val !== undefined && val !== null) {
      out[p.key] = val;
    } else if (p.param_type === "number") {
      out[p.key] = typeof p.min === "number" ? p.min : 0;
    } else if (p.param_type === "boolean") {
      out[p.key] = false;
    } else if (p.param_type === "select") {
      out[p.key] = p.options[0]?.value ?? "";
    } else {
      out[p.key] = "";
    }
  }
  return out;
}

function cloneJsonSafeValue(raw: unknown): unknown {
  if (raw === null) return null;
  if (typeof raw === "number") {
    return Number.isFinite(raw) ? (Object.is(raw, -0) ? 0 : raw) : undefined;
  }
  if (typeof raw === "string" || typeof raw === "boolean") {
    return raw;
  }
  if (Array.isArray(raw)) {
    const out: unknown[] = [];
    for (const item of raw) {
      const cloned = cloneJsonSafeValue(item);
      if (cloned !== undefined) out.push(cloned);
    }
    return out;
  }
  if (typeof raw === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
      const cloned = cloneJsonSafeValue(v);
      if (cloned !== undefined) out[k] = cloned;
    }
    return out;
  }
  return undefined;
}

export function normalizeWidgetPluginConfig(
  raw: unknown,
): Record<string, unknown> | undefined {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return undefined;
  }
  const cloned = cloneJsonSafeValue(raw);
  if (!cloned || typeof cloned !== "object" || Array.isArray(cloned)) {
    return undefined;
  }
  return cloned as Record<string, unknown>;
}

function injectWidgetPluginStylesheet(pluginId: string, styleCss: string | null): void {
  if (typeof document === "undefined" || !document.head) return;
  const selector = `style[data-ddscope-widget-plugin="${pluginId}"]`;
  const existing = document.head.querySelector<HTMLStyleElement>(selector);
  if (!styleCss || styleCss.trim().length === 0) {
    existing?.remove();
    return;
  }
  if (existing) {
    existing.textContent = styleCss;
    return;
  }
  const el = document.createElement("style");
  el.setAttribute("data-ddscope-widget-plugin", pluginId);
  el.textContent = styleCss;
  document.head.appendChild(el);
}

export function loadMountedWidgetPluginModule(
  pluginId: string,
  fetchBundle: (id: string) => Promise<WidgetPluginBundleDto_Serialize>,
): Promise<ExternalWidgetPluginModule> {
  const existing = loadedModuleCache.get(pluginId);
  if (existing) return existing;

  const loadPromise = (async () => {
    const bundle = await fetchBundle(pluginId);
    injectWidgetPluginStylesheet(pluginId, bundle.style_css ?? null);
    const blob = new Blob([bundle.entry_js], { type: "text/javascript" });
    const blobUrl = URL.createObjectURL(blob);
    try {
      const mod = (await import(/* @vite-ignore */ blobUrl)) as Partial<ExternalWidgetPluginModule>;
      if (typeof mod.mount !== "function") {
        throw new Error(
          `External widget plugin '${pluginId}' entry module must export mount(container, api).`,
        );
      }
      return mod as ExternalWidgetPluginModule;
    } finally {
      URL.revokeObjectURL(blobUrl);
    }
  })().catch((err) => {
    loadedModuleCache.delete(pluginId);
    throw err;
  });

  loadedModuleCache.set(pluginId, loadPromise);
  return loadPromise;
}
