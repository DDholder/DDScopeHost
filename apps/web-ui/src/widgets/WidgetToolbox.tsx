import { useState, type DragEvent } from "react";
import type { UiLocale } from "../i18n/index.ts";
import { translate, type MessageKey } from "../i18n/index.ts";
import { WIDGET_CATALOG, type WidgetType } from "./types.ts";

export interface WidgetToolboxProps {
  locale: UiLocale;
  onQuickAddWidget: (widgetType: WidgetType) => void;
}

export function WidgetToolbox({ locale, onQuickAddWidget }: WidgetToolboxProps) {
  const [collapsed, setCollapsed] = useState(false);

  const t = (key: MessageKey, params?: Record<string, string | number>) =>
    translate(locale, key, params);

  const handleDragStart = (ev: DragEvent<HTMLDivElement>, widgetType: WidgetType) => {
    ev.dataTransfer.setData(
      "application/x-ddscope-widget",
      JSON.stringify({ widgetType }),
    );
    ev.dataTransfer.setData("text/plain", `widget:${widgetType}`);
    ev.dataTransfer.effectAllowed = "copy";
  };

  return (
    <section className="widget-toolbox-section" data-testid="widget-toolbox">
      <div className="widget-toolbox-header">
        <button
          type="button"
          className="widget-toolbox-toggle"
          onClick={() => setCollapsed((v) => !v)}
        >
          <span className="group-caret">{collapsed ? "▸" : "▾"}</span>
          <span className="sidebar-title">{t("toolbox.title")}</span>
        </button>
        <span className="widget-toolbox-hint">{t("toolbox.hint")}</span>
      </div>

      {!collapsed && (
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
      )}
    </section>
  );
}
