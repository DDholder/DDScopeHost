import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";

const WidgetHeaderSlotContext = createContext<HTMLDivElement | null>(null);

export const WidgetHeaderSlotProvider = WidgetHeaderSlotContext.Provider;

/**
 * Portals interactive widget header controls (channel selector, command selector,
 * action buttons, and config toggle) directly into the enclosing card's title bar.
 */
export function WidgetHeaderActions({ children }: { children: ReactNode }) {
  const host = useContext(WidgetHeaderSlotContext);
  if (host) {
    return createPortal(children, host);
  }
  return <div className="widget-header-actions-fallback">{children}</div>;
}
