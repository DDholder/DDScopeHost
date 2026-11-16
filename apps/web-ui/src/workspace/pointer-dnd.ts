import type { PointerEvent as ReactPointerEvent } from "react";
import type { WidgetType } from "../widgets/types.ts";

export const DDSCOPE_POINTER_DROP_EVENT = "ddscope-pointer-drop";

export type PointerDragPayload =
  | {
      kind: "channel";
      channelId: number;
      modality: string;
      fullPath: string;
      label: string;
    }
  | {
      kind: "widget";
      widgetType: WidgetType;
      badge: string;
      label: string;
    }
  | {
      kind: "command";
      commandId: string;
      argIndex?: number;
      badge: string;
      label: string;
    };

export type DropTargetKind =
  | "widget-card"
  | "scope-pane"
  | "canvas-grid"
  | "watermark";

export interface PointerDropEventDetail {
  payload: PointerDragPayload;
  dropKind: DropTargetKind;
  clientX: number;
  clientY: number;
  widgetId?: string;
  canvasId?: number;
  scopeId?: number;
}

interface ResolvedDropTarget {
  el: HTMLElement;
  kind: DropTargetKind;
  highlightClass: string;
  widgetId?: string;
  canvasId?: number;
  scopeId?: number;
}

let pointerDragArmed = false;
let activeCleanup: (() => void) | null = null;

export function isPointerDragArmed(): boolean {
  return pointerDragArmed;
}

/**
 * Extracts a dragged command ID and optional placeholder slot index (`0..N` or `-1`)
 * from an HTML5 drag event's `DataTransfer`.
 */
export function parseCommandDragTransfer(ev: {
  dataTransfer: DataTransfer;
}): { commandId: string; argIndex?: number } | null {
  const rawJson = ev.dataTransfer.getData("application/x-ddscope-command");
  if (rawJson) {
    try {
      const parsed = JSON.parse(rawJson) as { commandId?: string; argIndex?: number };
      if (typeof parsed.commandId === "string" && parsed.commandId.trim().length > 0) {
        return {
          commandId: parsed.commandId.trim(),
          ...(typeof parsed.argIndex === "number" && Number.isFinite(parsed.argIndex)
            ? { argIndex: Math.trunc(parsed.argIndex) }
            : {}),
        };
      }
    } catch {
      // Ignore malformed command payload
    }
  }
  const plain = ev.dataTransfer.getData("text/plain").trim();
  if (plain.startsWith("command:")) {
    const rest = plain.slice("command:".length).trim();
    const match = /^(.+?)@(-?\d+)$/.exec(rest);
    if (match && match[1].trim().length > 0) {
      return {
        commandId: match[1].trim(),
        argIndex: Number.parseInt(match[2], 10),
      };
    }
    return rest.length > 0 ? { commandId: rest } : null;
  }
  return null;
}

function findFirstMatching(stack: Element[], selector: string): HTMLElement | null {
  for (let i = 0; i < stack.length; i += 1) {
    const matched = stack[i].closest<HTMLElement>(selector);
    if (matched) return matched;
  }
  return null;
}

function resolveDropTarget(
  clientX: number,
  clientY: number,
  payload: PointerDragPayload,
): ResolvedDropTarget | null {
  const stack = document.elementsFromPoint(clientX, clientY);

  if (payload.kind === "channel") {
    const widgetCard = findFirstMatching(stack, "[data-drop-widget-id]");
    if (widgetCard) {
      const widgetId = widgetCard.getAttribute("data-drop-widget-id") ?? undefined;
      const rawCanvasId = widgetCard.getAttribute("data-drop-canvas-id");
      const canvasId = rawCanvasId ? Number.parseInt(rawCanvasId, 10) : undefined;
      return {
        el: widgetCard,
        kind: "widget-card",
        highlightClass: "is-channel-drop-over",
        widgetId,
        canvasId: Number.isFinite(canvasId) ? canvasId : undefined,
      };
    }

    const scopePane = findFirstMatching(stack, "[data-drop-scope-id]");
    if (scopePane) {
      const rawScopeId = scopePane.getAttribute("data-drop-scope-id");
      const scopeId = rawScopeId ? Number.parseInt(rawScopeId, 10) : undefined;
      return {
        el: scopePane,
        kind: "scope-pane",
        highlightClass: "scope-widget-drop-active",
        scopeId: Number.isFinite(scopeId) ? scopeId : undefined,
      };
    }
  }

  if (payload.kind === "command") {
    const cmdWidgetCard = findFirstMatching(stack, "[data-drop-command-widget-id]");
    if (cmdWidgetCard) {
      const widgetId =
        cmdWidgetCard.getAttribute("data-drop-command-widget-id") ?? undefined;
      const rawCanvasId = cmdWidgetCard.getAttribute("data-drop-canvas-id");
      const canvasId = rawCanvasId ? Number.parseInt(rawCanvasId, 10) : undefined;
      return {
        el: cmdWidgetCard,
        kind: "widget-card",
        highlightClass: "is-channel-drop-over",
        widgetId,
        canvasId: Number.isFinite(canvasId) ? canvasId : undefined,
      };
    }
  }

  const canvasGrid = findFirstMatching(stack, "[data-drop-canvas-grid]");
  if (canvasGrid) {
    const rawCanvasId = canvasGrid.getAttribute("data-drop-canvas-id");
    const canvasId = rawCanvasId ? Number.parseInt(rawCanvasId, 10) : undefined;
    return {
      el: canvasGrid,
      kind: "canvas-grid",
      highlightClass: "is-drag-over",
      canvasId: Number.isFinite(canvasId) ? canvasId : undefined,
    };
  }

  if (payload.kind === "widget" || payload.kind === "command") {
    const watermark = findFirstMatching(stack, "[data-drop-watermark]");
    if (watermark) {
      return {
        el: watermark,
        kind: "watermark",
        highlightClass: "is-drag-over",
      };
    }
  }

  return null;
}

function createGhostElement(payload: PointerDragPayload): HTMLDivElement {
  const ghost = document.createElement("div");
  ghost.className = "ddscope-drag-ghost";

  const badge = document.createElement("span");
  badge.className = "ddscope-drag-ghost-badge";
  badge.textContent = payload.kind === "channel" ? "CH" : payload.badge;

  const text = document.createElement("span");
  text.textContent = payload.label;

  ghost.append(badge, text);
  document.body.appendChild(ghost);
  return ghost;
}

export function startPointerDrag(
  ev: ReactPointerEvent<HTMLElement>,
  payload: PointerDragPayload,
): void {
  if (ev.button !== 0) return;
  const targetEl = ev.target as HTMLElement | null;
  if (targetEl?.closest("button, input, select, textarea, a")) return;

  ev.preventDefault();
  activeCleanup?.();

  const startX = ev.clientX;
  const startY = ev.clientY;
  let dragging = false;
  let ghostEl: HTMLDivElement | null = null;
  let highlighted: ResolvedDropTarget | null = null;
  const prevUserSelect = document.body.style.userSelect;
  const prevCursor = document.body.style.cursor;
  pointerDragArmed = true;

  const clearHighlight = () => {
    if (highlighted) {
      highlighted.el.classList.remove(highlighted.highlightClass);
      highlighted = null;
    }
  };

  const cleanup = () => {
    pointerDragArmed = false;
    clearHighlight();
    if (ghostEl) {
      ghostEl.remove();
      ghostEl = null;
    }
    if (dragging) {
      document.body.style.userSelect = prevUserSelect;
      document.body.style.cursor = prevCursor;
    }
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", onPointerUp);
    window.removeEventListener("pointercancel", onPointerCancel);
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("blur", onWindowBlur);
    if (activeCleanup === cleanup) {
      activeCleanup = null;
    }
  };

  const positionGhost = (clientX: number, clientY: number) => {
    if (!ghostEl) return;
    ghostEl.style.transform = `translate3d(${Math.round(clientX + 14)}px, ${Math.round(clientY + 12)}px, 0)`;
  };

  const onPointerMove = (moveEv: PointerEvent) => {
    if (!dragging) {
      if (Math.hypot(moveEv.clientX - startX, moveEv.clientY - startY) < 5) {
        return;
      }
      dragging = true;
      document.body.style.userSelect = "none";
      document.body.style.cursor = "grabbing";
      ghostEl = createGhostElement(payload);
    }

    positionGhost(moveEv.clientX, moveEv.clientY);

    const nextTarget = resolveDropTarget(moveEv.clientX, moveEv.clientY, payload);
    if (
      highlighted &&
      (!nextTarget ||
        nextTarget.el !== highlighted.el ||
        nextTarget.highlightClass !== highlighted.highlightClass)
    ) {
      clearHighlight();
    }
    if (nextTarget) {
      nextTarget.el.classList.add(nextTarget.highlightClass);
      highlighted = nextTarget;
    }
  };

  const onPointerUp = (upEv: PointerEvent) => {
    const wasDragging = dragging;
    cleanup();
    if (!wasDragging) return;

    const dropTarget = resolveDropTarget(upEv.clientX, upEv.clientY, payload);
    if (!dropTarget) return;

    dropTarget.el.dispatchEvent(
      new CustomEvent<PointerDropEventDetail>(DDSCOPE_POINTER_DROP_EVENT, {
        bubbles: true,
        detail: {
          payload,
          dropKind: dropTarget.kind,
          clientX: upEv.clientX,
          clientY: upEv.clientY,
          widgetId: dropTarget.widgetId,
          canvasId: dropTarget.canvasId,
          scopeId: dropTarget.scopeId,
        },
      }),
    );
  };

  const onPointerCancel = () => {
    cleanup();
  };

  const onWindowBlur = () => {
    cleanup();
  };

  const onKeyDown = (keyEv: KeyboardEvent) => {
    if (keyEv.key === "Escape") {
      cleanup();
    }
  };

  activeCleanup = cleanup;
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp);
  window.addEventListener("pointercancel", onPointerCancel);
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("blur", onWindowBlur);
}
