import { useCallback } from "react";
import { translate, type MessageKey } from "../i18n/index.ts";
import { useWorkspaceStore } from "./store.ts";

export function useI18n() {
  const locale = useWorkspaceStore((s) => s.locale);
  const setLocale = useWorkspaceStore((s) => s.setLocale);

  const t = useCallback(
    (key: MessageKey, params?: Record<string, string | number>) =>
      translate(locale, key, params),
    [locale],
  );

  return { locale, setLocale, t };
}
