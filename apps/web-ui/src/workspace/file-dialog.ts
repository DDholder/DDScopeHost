interface SaveFilePickerWindow {
  showSaveFilePicker?: (options?: {
    suggestedName?: string;
    types?: Array<{
      description?: string;
      accept: Record<string, string[]>;
    }>;
  }) => Promise<{
    createWritable: () => Promise<{
      write: (data: string) => Promise<void>;
      close: () => Promise<void>;
    }>;
  }>;
}

export function sanitizeFilenameSlug(name: string): string {
  const cleaned = name
    .trim()
    .replace(/[<>:"/\\|?*\x00-\x1f]/g, "-")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
  return cleaned.length > 0 ? cleaned.slice(0, 64) : "config";
}

export async function saveJsonFile(
  jsonContent: string,
  suggestedFilename: string,
): Promise<boolean> {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return false;
  }

  const pickerWin = window as unknown as SaveFilePickerWindow;
  if (typeof pickerWin.showSaveFilePicker === "function") {
    try {
      const handle = await pickerWin.showSaveFilePicker({
        suggestedName: suggestedFilename,
        types: [
          {
            description: "JSON Configuration",
            accept: { "application/json": [".json"] },
          },
        ],
      });
      const writable = await handle.createWritable();
      await writable.write(jsonContent);
      await writable.close();
      return true;
    } catch (err: unknown) {
      if (
        typeof err === "object" &&
        err !== null &&
        "name" in err &&
        (err as { name?: string }).name === "AbortError"
      ) {
        return false;
      }
      // Fall through to anchor download if File System Access API fails unexpectedly
    }
  }

  const blob = new Blob([jsonContent], { type: "application/json;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = suggestedFilename;
  anchor.style.display = "none";
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  return true;
}

export async function readJsonFileFromInput(target: HTMLInputElement): Promise<string | null> {
  const file = target.files?.[0] ?? null;
  target.value = "";
  if (!file) return null;
  return file.text();
}
