import { useState, useSyncExternalStore, type DragEvent } from "react";
import type { UiLocale } from "../i18n/index.ts";
import { translate, type MessageKey } from "../i18n/index.ts";
import { isPointerDragArmed, startPointerDrag } from "../workspace/pointer-dnd.ts";
import {
  getExternalPluginCatalog,
  resolvePluginText,
  subscribeExternalPluginCatalog,
  toPluginWidgetType,
} from "./plugin-registry.ts";
import { WIDGET_CATALOG, type WidgetType } from "./types.ts";

export interface WidgetToolboxProps {
  locale: UiLocale;
  onQuickAddWidget: (widgetType: WidgetType) => void;
  onReloadPlugins?: () => Promise<void>;
}

export function WidgetToolbox({
  locale,
  onQuickAddWidget,
  onReloadPlugins,
}: WidgetToolboxProps) {
  const t = (key: MessageKey, params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const catalog = useSyncExternalStore(
    subscribeExternalPluginCatalog,
    getExternalPluginCatalog,
    getExternalPluginCatalog,
  );
  const [reloading, setReloading] = useState(false);

  const handleDragStart = (ev: DragEvent<HTMLDivElement>, widgetType: WidgetType) => {
    if (isPointerDragArmed()) {
      ev.preventDefault();
      return;
    }
    ev.dataTransfer.setData(
      "application/x-ddscope-widget",
      JSON.stringify({ widgetType }),
    );
    ev.dataTransfer.setData("text/plain", `widget:${widgetType}`);
    ev.dataTransfer.effectAllowed = "copy";
  };

  const handleReload = async () => {
    if (!onReloadPlugins || reloading) return;
    setReloading(true);
    try {
      await onReloadPlugins();
    } finally {
      setReloading(false);
    }
  };

  return (
    <section className="widget-toolbox-section" data-testid="widget-toolbox">
      <div className="widget-toolbox-header">
        <span className="sidebar-title">{t("toolbox.title")}</span>
        <span className="widget-toolbox-hint">{t("toolbox.hint")}</span>
      </div>

      <div className="widget-toolbox-scroll">
        <div className="toolbox-grid">
          {WIDGET_CATALOG.map((entry) => {
            const nameKey = `toolbox.widget.${entry.type}.name` as MessageKey;
            const descKey = `toolbox.widget.${entry.type}.desc` as MessageKey;
            return (
              <div
                key={entry.type}
                className="toolbox-item"
                draggable
                data-testid={`toolbox-item-${entry.type}`}
                title={t(descKey)}
                onPointerDown={(ev) =>
                  startPointerDrag(ev, {
                    kind: "widget",
                    widgetType: entry.type,
                    badge: entry.badge,
                    label: t(nameKey),
                  })
                }
                onDragStart={(ev) => handleDragStart(ev, entry.type)}
              >
                <span className="toolbox-badge">{entry.badge}</span>
                <div className="toolbox-item-text">
                  <span className="toolbox-item-name">{t(nameKey)}</span>
                  <span className="toolbox-item-desc">{t(descKey)}</span>
                </div>
                <button
                  type="button"
                  className="toolbox-add-btn"
                  title={t("toolbox.addBtnTitle")}
                  data-testid={`toolbox-add-${entry.type}`}
                  onClick={(ev) => {
                    ev.stopPropagation();
                    onQuickAddWidget(entry.type);
                  }}
                >
                  +
                </button>
              </div>
            );
          })}
        </div>

        <div className="toolbox-plugins-header">
          <span className="toolbox-plugins-title">{t("toolbox.pluginsTitle")}</span>
          {onReloadPlugins && (
            <button
              type="button"
              className="toolbox-plugins-reload"
              disabled={reloading}
              onClick={() => void handleReload()}
              title={t("toolbox.reloadPlugins")}
              data-testid="toolbox-reload-plugins"
            >
              {reloading ? "…" : "↻"}
            </button>
          )}
        </div>

        {catalog.widgets.length === 0 ? (
          <div className="toolbox-plugins-empty">{t("toolbox.pluginsEmpty")}</div>
        ) : (
          <div className="toolbox-grid">
            {catalog.widgets.map((plugin) => {
              const widgetType = toPluginWidgetType(plugin.id);
              const name =
                resolvePluginText(plugin.name_en, plugin.name_zh, locale) || plugin.id;
              const desc =
                resolvePluginText(
                  plugin.description_en,
                  plugin.description_zh,
                  locale,
                ) || `v${plugin.version}`;
              const disabled = !plugin.available;
              return (
                <div
                  key={widgetType}
                  className={`toolbox-item toolbox-item-plugin ${disabled ? "is-disabled" : ""}`}
                  draggable={!disabled}
                  data-testid={`toolbox-item-${widgetType}`}
                  title={disabled && plugin.error ? plugin.error : desc}
                  onPointerDown={
                    disabled
                      ? undefined
                      : (ev) =>
                          startPointerDrag(ev, {
                            kind: "widget",
                            widgetType,
                            badge: plugin.badge,
                            label: name,
                          })
                  }
                  onDragStart={
                    disabled ? undefined : (ev) => handleDragStart(ev, widgetType)
                  }
                >
                  <span className="toolbox-badge toolbox-badge-plugin">
                    {plugin.badge}
                  </span>
                  <div className="toolbox-item-text">
                    <span className="toolbox-item-name">{name}</span>
                    <span className="toolbox-item-desc">
                      {disabled && plugin.error ? plugin.error : desc}
                    </span>
                  </div>
                  <button
                    type="button"
                    className="toolbox-add-btn"
                    disabled={disabled}
                    title={
                      disabled && plugin.error ? plugin.error : t("toolbox.addBtnTitle")
                    }
                    data-testid={`toolbox-add-${widgetType}`}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      if (!disabled) {
                        onQuickAddWidget(widgetType);
                      }
                    }}
                  >
                    +
                  </button>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </section>
  );
}
