import { useRef, useState } from "react";
import { useWorkspaceStore } from "./store.ts";
import { useI18n } from "./use-i18n.ts";

export function WorkspacePageBar() {
  const { t } = useI18n();
  const pages = useWorkspaceStore((s) => s.pages);
  const pageOrder = useWorkspaceStore((s) => s.pageOrder);
  const activePageId = useWorkspaceStore((s) => s.activePageId);
  const addPage = useWorkspaceStore((s) => s.addPage);
  const removePage = useWorkspaceStore((s) => s.removePage);
  const setActivePage = useWorkspaceStore((s) => s.setActivePage);
  const renamePage = useWorkspaceStore((s) => s.renamePage);

  const [editingPageId, setEditingPageId] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const skipBlurRef = useRef(false);

  const startRename = (pageId: number, curTitle: string) => {
    skipBlurRef.current = false;
    setDraft(curTitle);
    setEditingPageId(pageId);
  };

  const commitRename = (pageId: number, curTitle: string) => {
    if (skipBlurRef.current) {
      skipBlurRef.current = false;
      return;
    }
    setEditingPageId(null);
    const trimmed = draft.trim();
    if (trimmed && trimmed !== curTitle) {
      renamePage(pageId, trimmed);
    }
  };

  return (
    <nav
      className="workspace-page-bar"
      data-testid="workspace-page-bar"
      aria-label="Workspace Pages"
    >
      <div className="workspace-page-tabs-scroll" role="tablist">
        {pageOrder.map((pid) => {
          const pg = pages[pid];
          if (!pg) return null;
          const isActive = pg.pageId === activePageId;
          const panelCount = pg.scopeOrder.length + pg.canvasOrder.length;
          const isLast = pageOrder.length <= 1;

          return (
            <div
              key={pg.pageId}
              role="tab"
              aria-selected={isActive}
              className={`workspace-page-tab ${isActive ? "active" : ""}`}
              data-testid={`page-tab-${pg.pageId}`}
              onClick={() => setActivePage(pg.pageId)}
            >
              {editingPageId === pg.pageId ? (
                <input
                  type="text"
                  className="inline-rename-input workspace-page-rename-input"
                  data-testid={`page-rename-input-${pg.pageId}`}
                  value={draft}
                  autoFocus
                  onFocus={(ev) => ev.currentTarget.select()}
                  onChange={(ev) => setDraft(ev.target.value)}
                  onBlur={() => commitRename(pg.pageId, pg.title)}
                  onClick={(ev) => ev.stopPropagation()}
                  onDoubleClick={(ev) => ev.stopPropagation()}
                  onKeyDown={(ev) => {
                    ev.stopPropagation();
                    if (ev.key === "Enter") {
                      ev.preventDefault();
                      commitRename(pg.pageId, pg.title);
                    } else if (ev.key === "Escape") {
                      ev.preventDefault();
                      skipBlurRef.current = true;
                      setEditingPageId(null);
                    }
                  }}
                />
              ) : (
                <>
                  <span
                    className="workspace-page-title"
                    title={t("page.renameTitle")}
                    onDoubleClick={(ev) => {
                      ev.stopPropagation();
                      startRename(pg.pageId, pg.title);
                    }}
                  >
                    {pg.title}
                  </span>
                  <span className="workspace-page-badge">{panelCount}</span>
                  <button
                    type="button"
                    className="workspace-page-action-btn"
                    title={t("page.renameTitle")}
                    data-testid={`page-rename-btn-${pg.pageId}`}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      startRename(pg.pageId, pg.title);
                    }}
                  >
                    ✎
                  </button>
                  <button
                    type="button"
                    className="workspace-page-action-btn workspace-page-close-btn"
                    title={isLast ? t("page.keepLastTitle") : t("page.closeTitle")}
                    data-testid={`page-close-${pg.pageId}`}
                    disabled={isLast}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      removePage(pg.pageId);
                    }}
                  >
                    ×
                  </button>
                </>
              )}
            </div>
          );
        })}
      </div>

      <button
        type="button"
        className="scope-btn scope-btn-accent workspace-page-add-btn"
        title={t("page.addTitle")}
        data-testid="add-page-btn"
        onClick={() => addPage()}
      >
        {t("page.add")}
      </button>
    </nav>
  );
}
