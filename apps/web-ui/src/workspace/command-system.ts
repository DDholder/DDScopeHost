import type { DownlinkPayloadDto } from "../bridge/bindings.ts";

export type CommandFormat = "string" | "hex";
export type ByteEndian = "little" | "big";

export interface CommandDefinition {
  id: string;
  name: string;
  format: CommandFormat;
  template: string;
  appendNewline: boolean;
  endian: ByteEndian;
  targetSourceId: string;
  priority: "normal" | "high";
}

export type CompiledCommandResult =
  | { ok: true; payload: DownlinkPayloadDto; preview: string; usesValue: boolean }
  | { ok: false; error: string; usesValue: boolean };

const STRING_SPECIFIER_RE = /^%([+\-0 ]*)(\d+)?(?:\.(\d+))?(lf|f|e|E|g|d|i|u|x|X)/;
const STRING_ANY_SPECIFIER_SEARCH_RE = /%([+\-0 ]*)(\d+)?(?:\.(\d+))?(lf|f|e|E|g|d|i|u|x|X)/;
const HEX_PLACEHOLDER_RE = /^%(lf|hd|hu|hhd|hhu|0?[248]?[xX]|[fdiub])$/;
const HEX_SEGMENT_TOKEN_RE = /%(?:lf|hd|hu|hhd|hhu|0?[248]?[xX]|[fdiub])|(?:0[xX])?[0-9a-fA-F]+/g;
const HEX_PLACEHOLDER_SEARCH_RE = /%(?:lf|hd|hu|hhd|hhu|0?[248]?[xX]|[fdiub])/;

function clampInt(val: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.trunc(val)));
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

export function commandUsesValue(
  cmd: Pick<CommandDefinition, "format" | "template">,
): boolean {
  const stripped = cmd.template.replace(/%%/g, "");
  if (cmd.format === "hex") {
    return HEX_PLACEHOLDER_SEARCH_RE.test(stripped);
  }
  return STRING_ANY_SPECIFIER_SEARCH_RE.test(stripped);
}

export function formatStringCommand(
  template: string,
  value: number,
): { ok: true; text: string; usesValue: boolean } | { ok: false; error: string; usesValue: boolean } {
  const unescaped = unescapeStringTemplate(template);
  const safeVal = Number.isFinite(value) ? (Object.is(value, -0) ? 0 : value) : 0;
  let usesValue = false;
  let out = "";
  let idx = 0;

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
    const [fullMatch, flags = "", widthStr, precStr, spec] = match;
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
  if (!HEX_PLACEHOLDER_RE.test(token)) {
    return null;
  }
  const spec = token.slice(1);
  const safeVal = Number.isFinite(value) ? (Object.is(value, -0) ? 0 : value) : 0;
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
  value: number,
  endian: ByteEndian,
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
        const encoded = encodeHexPlaceholderBytes(token, value, endian);
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
  value = 0,
): CompiledCommandResult {
  if (cmd.format === "hex") {
    const res = formatHexCommand(cmd.template, value, cmd.endian);
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

  const res = formatStringCommand(cmd.template, value);
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

  return {
    id,
    name,
    format,
    template,
    appendNewline,
    endian,
    targetSourceId,
    priority,
  };
}
