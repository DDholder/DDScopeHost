import { useEffect, type ReactNode } from "react";
import { createPortal } from "react-dom";

export interface WidgetConfigModalProps {
  open: boolean;
  title: string;
  doneLabel: string;
  width?: "sm" | "md" | "lg" | "xl";
  dataTestId?: string;
  onClose: () => void;
  children: ReactNode;
}

/**
 * Portal-backed centered modal dialog for widget settings so even compact
 * dashboard cards present unconstrained configuration and channel-binding UI.
 */
export function WidgetConfigModal({
  open,
  title,
  doneLabel,
  width = "sm",
  dataTestId,
  onClose,
  children,
}: WidgetConfigModalProps) {
  useEffect(() => {
    if (!open || typeof window === "undefined") return;
    const onKeyDown = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [open, onClose]);

  if (!open || typeof document === "undefined") {
    return null;
  }

  return createPortal(
    <div
      className="widget-config-modal-backdrop"
      onPointerDown={(ev) => ev.stopPropagation()}
      onMouseDown={(ev) => ev.stopPropagation()}
      onClick={(ev) => {
        ev.stopPropagation();
        if (ev.target === ev.currentTarget) {
          onClose();
        }
      }}
      onDragOver={(ev) => {
        ev.preventDefault();
        ev.stopPropagation();
      }}
      onDrop={(ev) => {
        ev.preventDefault();
        ev.stopPropagation();
      }}
    >
      <div
        className={`widget-config-modal-panel size-${width}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        data-testid={dataTestId}
        onPointerDown={(ev) => ev.stopPropagation()}
        onMouseDown={(ev) => ev.stopPropagation()}
        onClick={(ev) => ev.stopPropagation()}
      >
        <div className="widget-config-modal-header">
          <span className="widget-config-modal-title" title={title}>
            {title}
          </span>
          <button
            type="button"
            className="widget-config-modal-close"
            aria-label={doneLabel}
            onClick={onClose}
          >
            ✕
          </button>
        </div>

        <div className="widget-config-modal-body">{children}</div>

        <div className="widget-config-modal-footer">
          <button
            type="button"
            className="scope-btn scope-btn-accent"
            onClick={onClose}
          >
            {doneLabel}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
