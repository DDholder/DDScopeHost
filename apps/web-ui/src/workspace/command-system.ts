import type { DownlinkPayloadDto } from "../bridge/bindings.ts";
import type { DashboardCanvasConfig, WidgetCmdTableRow } from "../widgets/types.ts";

export type CommandFormat = "string" | "hex";
export type ByteEndian = "little" | "big";

export interface CommandPlaceholderInfo {
  /** 0-based placeholder slot index (`0` -> `#1`, `1` -> `#2`, ...). */
  index: number;
  /** Human-readable slot badge (`#1`, `#2`, ...). */
  label: string;
  /** Matched format token (e.g. `%f`, `%.2f`, `%hu`, `%2$d`). */
  token: string;
}

export type CommandValueInput = number | ReadonlyArray<number>;

export interface CommandDefinition {
  id: string;
  name: string;
  format: CommandFormat;
  template: string;
  appendNewline: boolean;
  endian: ByteEndian;
  targetSourceId: string;
  priority: "normal" | "high";
  /** Live numeric values for each placeholder slot `[arg0, arg1, ...]`. */
  argValues?: number[];
}

export type CompiledCommandResult =
  | { ok: true; payload: DownlinkPayloadDto; preview: string; usesValue: boolean }
  | { ok: false; error: string; usesValue: boolean };

const STRING_SPECIFIER_RE =
  /^%(?:([1-9]\d*)\$)?([+\-0 ]*)(\d+)?(?:\.(\d+))?(lf|f|e|E|g|d|i|u|x|X)/;
const HEX_PLACEHOLDER_RE =
  /^%(?:([1-9]\d*)\$)?(lf|hd|hu|hhd|hhu|0?[248]?[xX]|[fdiub])$/;
const HEX_SEGMENT_TOKEN_RE =
  /%(?:[1-9]\d*\$)?(?:lf|hd|hu|hhd|hhu|0?[248]?[xX]|[fdiub])|(?:0[xX])?[0-9a-fA-F]+/g;

function clampInt(val: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.trunc(val)));
}

function sanitizeFiniteNumber(val: unknown): number {
  if (typeof val !== "number" || !Number.isFinite(val)) {
    return 0;
  }
  return Object.is(val, -0) ? 0 : val;
}

function resolveSlotValue(values: CommandValueInput, slotIndex: number): number {
  if (typeof values === "number") {
    return sanitizeFiniteNumber(values);
  }
  return sanitizeFiniteNumber(values[slotIndex]);
}

function unescapeStringTemplate(raw: string): string {
  let out = "";
  for (let i = 0; i < raw.length; i += 1) {
    const ch = raw[i];
    if (ch === "\\" && i + 1 < raw.length) {
      const next = raw[i + 1];
      if (next === "n") {
        out += "\n";
        i += 1;
        continue;
      }
      if (next === "r") {
        out += "\r";
        i += 1;
        continue;
      }
      if (next === "t") {
        out += "\t";
        i += 1;
        continue;
      }
      if (next === "\\") {
        out += "\\";
        i += 1;
        continue;
      }
    }
    out += ch;
  }
  return out;
}

function applySignAndWidth(
  signStr: string,
  bodyStr: string,
  width: number,
  leftAlign: boolean,
  zeroPad: boolean,
): string {
  const totalLen = signStr.length + bodyStr.length;
  if (width <= totalLen) {
    return `${signStr}${bodyStr}`;
  }
  const padLen = width - totalLen;
  if (leftAlign) {
    return `${signStr}${bodyStr}${" ".repeat(padLen)}`;
  }
  if (zeroPad) {
    return `${signStr}${"0".repeat(padLen)}${bodyStr}`;
  }
  return `${" ".repeat(padLen)}${signStr}${bodyStr}`;
}

function formatFloatClean(val: number): string {
  const abs = Math.abs(val);
  if (abs === 0) {
    return "0";
  }
  if (abs >= 1e-4 && abs < 1e9) {
    return String(Number(abs.toFixed(6)));
  }
  return String(Number(abs.toPrecision(7)));
}

export function extractCommandPlaceholders(
  cmd: Pick<CommandDefinition, "format" | "template">,
): CommandPlaceholderInfo[] {
  const slots = new Map<number, string>();
  let nextSeq = 0;

  if (cmd.format === "hex") {
    const trimmed = cmd.template.trim();
    if (trimmed) {
      const segments = trimmed.split(/[\s,]+/).filter(Boolean);
      for (const segment of segments) {
        HEX_SEGMENT_TOKEN_RE.lastIndex = 0;
        let cursor = 0;
        while (cursor < segment.length) {
          const match = HEX_SEGMENT_TOKEN_RE.exec(segment);
          if (!match) {
            break;
          }
          if (match.index !== cursor) {
            cursor = match.index;
            HEX_SEGMENT_TOKEN_RE.lastIndex = cursor;
            continue;
          }
          const token = match[0];
          cursor += token.length;
          if (token.startsWith("%")) {
            const phMatch = HEX_PLACEHOLDER_RE.exec(token);
            if (phMatch) {
              const posStr = phMatch[1];
              const slotIndex = posStr
                ? clampInt(Number.parseInt(posStr, 10) - 1, 0, 63)
                : nextSeq++;
              if (!slots.has(slotIndex)) {
                slots.set(slotIndex, token);
              }
            }
          }
        }
      }
    }
  } else {
    const unescaped = unescapeStringTemplate(cmd.template);
    let idx = 0;
    while (idx < unescaped.length) {
      if (unescaped[idx] !== "%") {
        idx += 1;
        continue;
      }
      if (idx + 1 < unescaped.length && unescaped[idx + 1] === "%") {
        idx += 2;
        continue;
      }
      const slice = unescaped.slice(idx);
      const match = STRING_SPECIFIER_RE.exec(slice);
      if (!match) {
        idx += 1;
        continue;
      }
      const posStr = match[1];
      const slotIndex = posStr
        ? clampInt(Number.parseInt(posStr, 10) - 1, 0, 63)
        : nextSeq++;
      if (!slots.has(slotIndex)) {
        slots.set(slotIndex, match[0]);
      }
      idx += match[0].length;
    }
  }

  if (slots.size === 0) {
    return [];
  }

  let maxSlotIndex = 0;
  for (const slotIdx of slots.keys()) {
    if (slotIdx > maxSlotIndex) {
      maxSlotIndex = slotIdx;
    }
  }

  const ordered: CommandPlaceholderInfo[] = [];
  for (let i = 0; i <= maxSlotIndex; i += 1) {
    ordered.push({
      index: i,
      label: `#${i + 1}`,
      token: slots.get(i) ?? "%f",
    });
  }
  return ordered;
}

export function countCommandPlaceholders(
  cmd: Pick<CommandDefinition, "format" | "template">,
): number {
  return extractCommandPlaceholders(cmd).length;
}

export function commandUsesValue(
  cmd: Pick<CommandDefinition, "format" | "template">,
): boolean {
  return countCommandPlaceholders(cmd) > 0;
}

export function buildEffectiveCommandArgs(
  cmd: Pick<CommandDefinition, "format" | "template" | "argValues">,
  override?: { slotIndex?: number | null; value?: number; values?: ReadonlyArray<number> },
): number[] {
  const count = countCommandPlaceholders(cmd);
  const targetLen =
    override?.values !== undefined
      ? Math.max(count, override.values.length)
      : typeof override?.slotIndex === "number" && override.slotIndex >= 0
        ? Math.max(count, override.slotIndex + 1)
        : count;

  const args: number[] = Array.from({ length: targetLen }, (_, i) =>
    sanitizeFiniteNumber(cmd.argValues?.[i]),
  );

  if (override?.values !== undefined) {
    for (let i = 0; i < override.values.length; i += 1) {
      const v = override.values[i];
      if (typeof v === "number" && Number.isFinite(v)) {
        args[i] = Object.is(v, -0) ? 0 : v;
      }
    }
  } else if (
    typeof override?.slotIndex === "number" &&
    override.slotIndex >= 0 &&
    typeof override.value === "number" &&
    Number.isFinite(override.value)
  ) {
    args[override.slotIndex] = Object.is(override.value, -0) ? 0 : override.value;
  } else if (
    override?.slotIndex === undefined &&
    typeof override?.value === "number" &&
    Number.isFinite(override.value) &&
    args.length > 0
  ) {
    args[0] = Object.is(override.value, -0) ? 0 : override.value;
  }

  return args;
}

export function formatStringCommand(
  template: string,
  values: CommandValueInput = 0,
): { ok: true; text: string; usesValue: boolean } | { ok: false; error: string; usesValue: boolean } {
  const unescaped = unescapeStringTemplate(template);
  let usesValue = false;
  let out = "";
  let idx = 0;
  let nextSeq = 0;

  while (idx < unescaped.length) {
    const ch = unescaped[idx];
    if (ch !== "%") {
      out += ch;
      idx += 1;
      continue;
    }

    if (idx + 1 < unescaped.length && unescaped[idx + 1] === "%") {
      out += "%";
      idx += 2;
      continue;
    }

    const slice = unescaped.slice(idx);
    const match = STRING_SPECIFIER_RE.exec(slice);
    if (!match) {
      const sample = slice.slice(0, 12);
      return {
        ok: false,
        error: `Invalid format specifier near "${sample}"`,
        usesValue,
      };
    }

    usesValue = true;
    const [fullMatch, posStr, flags = "", widthStr, precStr, spec] = match;
    const slotIndex = posStr ? clampInt(Number.parseInt(posStr, 10) - 1, 0, 63) : nextSeq++;
    const safeVal = resolveSlotValue(values, slotIndex);
    const leftAlign = flags.includes("-");
    const zeroPad = flags.includes("0") && !leftAlign;
    const alwaysSign = flags.includes("+");
    const spaceSign = flags.includes(" ") && !alwaysSign;
    const width = widthStr ? clampInt(Number.parseInt(widthStr, 10), 0, 64) : 0;
    const hasPrec = precStr !== undefined;
    const prec = hasPrec ? clampInt(Number.parseInt(precStr, 10), 0, 12) : 6;

    let formatted = "";
    if (spec === "f" || spec === "lf" || spec === "g") {
      const signStr =
        safeVal < 0 ? "-" : alwaysSign ? "+" : spaceSign ? " " : "";
      const bodyStr = hasPrec
        ? Math.abs(safeVal).toFixed(prec)
        : formatFloatClean(safeVal);
      formatted = applySignAndWidth(signStr, bodyStr, width, leftAlign, zeroPad);
    } else if (spec === "e" || spec === "E") {
      const signStr =
        safeVal < 0 ? "-" : alwaysSign ? "+" : spaceSign ? " " : "";
      let bodyStr = Math.abs(safeVal).toExponential(hasPrec ? prec : 6);
      if (spec === "E") {
        bodyStr = bodyStr.replace("e", "E");
      }
      formatted = applySignAndWidth(signStr, bodyStr, width, leftAlign, zeroPad);
    } else if (spec === "d" || spec === "i") {
      const truncated = Math.trunc(safeVal);
      const intVal = truncated === 0 ? 0 : truncated;
      const signStr =
        intVal < 0 ? "-" : alwaysSign ? "+" : spaceSign ? " " : "";
      const bodyStr = Math.abs(intVal).toString(10);
      formatted = applySignAndWidth(signStr, bodyStr, width, leftAlign, zeroPad);
    } else if (spec === "u") {
      const uVal = Math.trunc(safeVal) >>> 0;
      formatted = applySignAndWidth("", uVal.toString(10), width, leftAlign, zeroPad);
    } else {
      const uVal = Math.trunc(safeVal) >>> 0;
      const hexStr =
        spec === "X" ? uVal.toString(16).toUpperCase() : uVal.toString(16);
      formatted = applySignAndWidth("", hexStr, width, leftAlign, zeroPad);
    }

    out += formatted;
    idx += fullMatch.length;
  }

  return { ok: true, text: out, usesValue };
}

function encodeHexPlaceholderBytes(
  token: string,
  value: number,
  endian: ByteEndian,
): number[] | null {
  const phMatch = HEX_PLACEHOLDER_RE.exec(token);
  if (!phMatch) {
    return null;
  }
  const spec = phMatch[2];
  const safeVal = sanitizeFiniteNumber(value);
  const littleEndian = endian === "little";
  const buf = new ArrayBuffer(8);
  const view = new DataView(buf);

  if (spec === "f") {
    view.setFloat32(0, safeVal, littleEndian);
    return Array.from(new Uint8Array(buf, 0, 4));
  }
  if (spec === "lf") {
    view.setFloat64(0, safeVal, littleEndian);
    return Array.from(new Uint8Array(buf, 0, 8));
  }
  if (spec === "d" || spec === "i") {
    view.setInt32(0, Math.trunc(safeVal) | 0, littleEndian);
    return Array.from(new Uint8Array(buf, 0, 4));
  }
  if (spec === "u") {
    view.setUint32(0, Math.trunc(safeVal) >>> 0, littleEndian);
    return Array.from(new Uint8Array(buf, 0, 4));
  }
  if (spec === "hd") {
    view.setInt16(0, Math.trunc(safeVal), littleEndian);
    return Array.from(new Uint8Array(buf, 0, 2));
  }
  if (spec === "hu") {
    view.setUint16(0, Math.trunc(safeVal), littleEndian);
    return Array.from(new Uint8Array(buf, 0, 2));
  }
  if (
    spec === "b" ||
    spec === "hhd" ||
    spec === "hhu" ||
    spec === "x" ||
    spec === "X" ||
    spec === "2x" ||
    spec === "2X" ||
    spec === "02x" ||
    spec === "02X"
  ) {
    return [Math.trunc(safeVal) & 0xff];
  }
  if (spec === "4x" || spec === "4X" || spec === "04x" || spec === "04X") {
    const v = Math.trunc(safeVal) & 0xffff;
    return [(v >>> 8) & 0xff, v & 0xff];
  }
  const v = Math.trunc(safeVal) >>> 0;
  return [(v >>> 24) & 0xff, (v >>> 16) & 0xff, (v >>> 8) & 0xff, v & 0xff];
}

export function formatHexCommand(
  template: string,
  values: CommandValueInput = 0,
  endian: ByteEndian = "little",
): { ok: true; bytes: number[]; hex: string; usesValue: boolean } | { ok: false; error: string; usesValue: boolean } {
  const trimmed = template.trim();
  if (!trimmed) {
    return {
      ok: false,
      error: "Hex template cannot be empty",
      usesValue: false,
    };
  }

  const bytes: number[] = [];
  let usesValue = false;
  let nextSeq = 0;
  const segments = trimmed.split(/[\s,]+/).filter(Boolean);

  for (const segment of segments) {
    HEX_SEGMENT_TOKEN_RE.lastIndex = 0;
    let cursor = 0;

    while (cursor < segment.length) {
      const match = HEX_SEGMENT_TOKEN_RE.exec(segment);
      if (!match || match.index !== cursor) {
        return {
          ok: false,
          error: `Invalid hex token near "${segment.slice(cursor, cursor + 12)}"`,
          usesValue,
        };
      }

      const token = match[0];
      cursor += token.length;

      if (token.startsWith("%")) {
        const phMatch = HEX_PLACEHOLDER_RE.exec(token);
        const slotIndex = phMatch?.[1]
          ? clampInt(Number.parseInt(phMatch[1], 10) - 1, 0, 63)
          : nextSeq++;
        const encoded = encodeHexPlaceholderBytes(
          token,
          resolveSlotValue(values, slotIndex),
          endian,
        );
        if (!encoded) {
          return {
            ok: false,
            error: `Unsupported hex placeholder "${token}"`,
            usesValue,
          };
        }
        usesValue = true;
        bytes.push(...encoded);
      } else {
        const cleanHex =
          token.startsWith("0x") || token.startsWith("0X") ? token.slice(2) : token;
        if (cleanHex.length === 0 || cleanHex.length % 2 !== 0) {
          return {
            ok: false,
            error: `Invalid hex token "${token}": expected even number of hex digits`,
            usesValue,
          };
        }
        for (let i = 0; i < cleanHex.length; i += 2) {
          const byteVal = Number.parseInt(cleanHex.slice(i, i + 2), 16);
          if (!Number.isFinite(byteVal)) {
            return {
              ok: false,
              error: `Invalid hex byte "${cleanHex.slice(i, i + 2)}"`,
              usesValue,
            };
          }
          bytes.push(byteVal);
        }
      }
    }
  }

  if (bytes.length === 0) {
    return {
      ok: false,
      error: "Hex template produced no bytes",
      usesValue,
    };
  }

  const hex = bytes.map((b) => b.toString(16).toUpperCase().padStart(2, "0")).join(" ");
  return { ok: true, bytes, hex, usesValue };
}

export function compileCommandPayload(
  cmd: Pick<CommandDefinition, "format" | "template" | "appendNewline" | "endian">,
  values: CommandValueInput = 0,
): CompiledCommandResult {
  if (cmd.format === "hex") {
    const res = formatHexCommand(cmd.template, values, cmd.endian);
    if (!res.ok) {
      return res;
    }
    return {
      ok: true,
      payload: { kind: "hex_bytes", hex: res.hex },
      preview: `HEX: ${res.hex}`,
      usesValue: res.usesValue,
    };
  }

  const res = formatStringCommand(cmd.template, values);
  if (!res.ok) {
    return res;
  }
  return {
    ok: true,
    payload: {
      kind: "ascii_text",
      text: res.text,
      append_newline: Boolean(cmd.appendNewline),
    },
    preview: cmd.appendNewline ? `${res.text}\\n` : res.text,
    usesValue: res.usesValue,
  };
}

export function normalizeCommandDefinition(
  raw: unknown,
  fallbackIndex: number,
): CommandDefinition {
  const rec =
    typeof raw === "object" && raw !== null && !Array.isArray(raw)
      ? (raw as Record<string, unknown>)
      : {};

  const id =
    typeof rec.id === "string" && rec.id.trim().length > 0
      ? rec.id.trim()
      : `cmd_${fallbackIndex}`;
  const name =
    typeof rec.name === "string" && rec.name.trim().length > 0
      ? rec.name.trim()
      : `CMD #${fallbackIndex}`;
  const format: CommandFormat = rec.format === "hex" ? "hex" : "string";
  const template =
    typeof rec.template === "string"
      ? rec.template
      : format === "hex"
        ? "00 01 02"
        : "123";
  const appendNewline =
    typeof rec.appendNewline === "boolean" ? rec.appendNewline : false;
  const endian: ByteEndian = rec.endian === "big" ? "big" : "little";
  const targetSourceId =
    typeof rec.targetSourceId === "string" ? rec.targetSourceId.trim() : "";
  const priority = rec.priority === "high" ? "high" : "normal";
  const argValues = Array.isArray(rec.argValues)
    ? rec.argValues.slice(0, 64).map((v) => sanitizeFiniteNumber(v))
    : [];

  return {
    id,
    name,
    format,
    template,
    appendNewline,
    endian,
    targetSourceId,
    priority,
    argValues,
  };
}

/**
 * Fills any missing `argValues[slot]` entries on normalized commands from bound
 * dashboard widgets (`param_slider` first, then `cmd_button` slots >= 0).
 */
export function backfillCommandArgValuesFromCanvases(
  commands: ReadonlyArray<CommandDefinition>,
  canvases: ReadonlyArray<Pick<DashboardCanvasConfig, "widgets">>,
): CommandDefinition[] {
  return commands.map((cmd) => {
    const count = countCommandPlaceholders(cmd);
    if (count === 0) {
      return cmd;
    }

    const baseArgs: Array<number | undefined> = Array.from(
      { length: count },
      (_, i) => {
        const v = cmd.argValues?.[i];
        return typeof v === "number" && Number.isFinite(v) ? (Object.is(v, -0) ? 0 : v) : undefined;
      },
    );

    if (baseArgs.every((v) => v !== undefined)) {
      return cmd;
    }

    for (const cv of canvases) {
      for (const w of cv.widgets) {
        if (w.sliderConfig?.commandId !== cmd.id) continue;
        const slot = clampInt(w.sliderConfig.commandArgIndex ?? 0, 0, count - 1);
        if (baseArgs[slot] === undefined && Number.isFinite(w.sliderConfig.value)) {
          baseArgs[slot] = Object.is(w.sliderConfig.value, -0) ? 0 : w.sliderConfig.value;
        }
      }
    }

    for (const cv of canvases) {
      for (const w of cv.widgets) {
        if (w.buttonConfig?.commandId !== cmd.id) continue;
        const rawSlot = w.buttonConfig.commandArgIndex ?? 0;
        if (rawSlot < 0) continue;
        const slot = clampInt(rawSlot, 0, count - 1);
        if (
          baseArgs[slot] === undefined &&
          typeof w.buttonConfig.commandValue === "number" &&
          Number.isFinite(w.buttonConfig.commandValue)
        ) {
          baseArgs[slot] = Object.is(w.buttonConfig.commandValue, -0)
            ? 0
            : w.buttonConfig.commandValue;
        }
      }
    }

    for (const cv of canvases) {
      for (const w of cv.widgets) {
        if (
          w.cmdTableConfig?.commandId !== cmd.id ||
          w.cmdTableConfig.rows.length === 0
        ) {
          continue;
        }
        const firstRow = w.cmdTableConfig.rows[0];
        for (let slot = 0; slot < count; slot += 1) {
          const val = firstRow.values[slot];
          if (baseArgs[slot] === undefined && typeof val === "number" && Number.isFinite(val)) {
            baseArgs[slot] = Object.is(val, -0) ? 0 : val;
          }
        }
      }
    }

    return {
      ...cmd,
      argValues: baseArgs.map((v) => v ?? 0),
    };
  });
}

/**
 * Allocates a new Command Table row, copying values from the last existing row
 * or falling back to `seedValues`.
 */
export function createCmdTableRow(
  existingRows: ReadonlyArray<WidgetCmdTableRow>,
  placeholderCount: number,
  seedValues?: ReadonlyArray<number>,
  locale: "zh-CN" | "en" = "zh-CN",
): WidgetCmdTableRow {
  const used = new Set(existingRows.map((r) => r.id));
  let seq = existingRows.length + 1;
  while (used.has(`r_${seq}`)) {
    seq += 1;
  }

  const templateVals =
    existingRows.length > 0
      ? existingRows[existingRows.length - 1].values
      : seedValues;
  const safeCount = Math.max(0, Math.trunc(placeholderCount));
  const values = Array.from({ length: safeCount }, (_, i) =>
    sanitizeFiniteNumber(templateVals?.[i]),
  );

  return {
    id: `r_${seq}`,
    label:
      locale === "zh-CN"
        ? `预设 ${existingRows.length + 1}`
        : `Preset ${existingRows.length + 1}`,
    values,
  };
}

/**
 * Returns a new row values array with `values[slotIndex]` updated, preserving
 * any extra columns beyond `slotIndex`.
 */
export function updateCmdTableRowValue(
  values: ReadonlyArray<number>,
  slotIndex: number,
  nextValue: number,
): number[] {
  const safeSlot = Math.max(0, Math.trunc(slotIndex));
  const next = values.map((v) => sanitizeFiniteNumber(v));
  while (next.length <= safeSlot) {
    next.push(0);
  }
  next[safeSlot] = sanitizeFiniteNumber(nextValue);
  return next;
}

/**
 * Compiles a single Command Table row against its bound `CommandDefinition`.
 */
export function compileCmdTableRow(
  cmd: Pick<CommandDefinition, "format" | "template" | "appendNewline" | "endian">,
  row: Pick<WidgetCmdTableRow, "values">,
): { args: number[]; compiled: CompiledCommandResult } {
  const count = countCommandPlaceholders(cmd);
  const args = Array.from({ length: count }, (_, i) =>
    sanitizeFiniteNumber(row.values[i]),
  );
  return {
    args,
    compiled: compileCommandPayload(cmd, count > 0 ? args : 0),
  };
}
