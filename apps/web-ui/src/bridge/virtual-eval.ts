import type {
  ChannelModalityDto,
  PowerOutputFlagsDto,
  StateLabelEntryDto_Serialize,
  VirtualNodeConfigDto_Deserialize,
  VirtualNodeConfigDto_Serialize,
} from "./bindings.ts";

export const VIRTUAL_SOURCE_ID = "virtual";

export type PowerMetricKey =
  | "freq"
  | "u_rms"
  | "i_rms"
  | "p"
  | "q1"
  | "q_tot"
  | "s"
  | "pf"
  | "phi_deg"
  | "u_pk"
  | "i_pk"
  | "energy_wh";

export interface VirtualOutputSpec {
  localPath: string;
  fullPath: string;
  displayName: string;
  unit: string | null;
  modality: ChannelModalityDto;
  scalarType: string;
  stateLabels: StateLabelEntryDto_Serialize[];
  powerMetricKey?: PowerMetricKey;
}

export interface CompiledTsFormula {
  inputChannels: string[];
  evalAt: (tSec: number, channelValues: number[]) => number;
}

function finiteOr(v: number | null | undefined, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function optionalFinite(v: number | null | undefined): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function ensureLocalSegment(name: string, label: string): void {
  const trimmed = name.trim();
  if (!trimmed) throw new Error(`${label} must not be empty`);
  if (trimmed.includes("/")) throw new Error(`${label} ('${trimmed}') must not contain '/'`);
  if (trimmed !== name) {
    throw new Error(`${label} ('${name}') must not have leading/trailing whitespace`);
  }
}

function ensureInputPath(path: string, label: string): void {
  const trimmed = path.trim();
  if (!trimmed || !trimmed.includes("/")) {
    throw new Error(`${label} ('${trimmed}') must be formatted as '<source_id>/<local_path>'`);
  }
}

export function normalizePowerFlags(
  flags?: Partial<PowerOutputFlagsDto> | null,
): PowerOutputFlagsDto {
  return {
    freq: flags?.freq ?? true,
    u_rms: flags?.u_rms ?? true,
    i_rms: flags?.i_rms ?? true,
    p: flags?.p ?? true,
    q1: flags?.q1 ?? true,
    q_tot: flags?.q_tot ?? true,
    s: flags?.s ?? true,
    pf: flags?.pf ?? true,
    phi_deg: flags?.phi_deg ?? true,
    u_pk: flags?.u_pk ?? true,
    i_pk: flags?.i_pk ?? true,
    energy_wh: flags?.energy_wh ?? true,
  };
}

export function normalizeVirtualNodeConfig(
  cfg: VirtualNodeConfigDto_Deserialize,
): VirtualNodeConfigDto_Serialize {
  const op = cfg.operator;
  let normOp: VirtualNodeConfigDto_Serialize["operator"];

  switch (op.kind) {
    case "analog_to_digital": {
      const m = op.mode;
      normOp = {
        kind: "analog_to_digital",
        input_channel: op.input_channel.trim(),
        output_path: op.output_path.trim(),
        mode:
          m.mode === "window"
            ? {
                mode: "window",
                low: finiteOr(m.low, -1),
                high: finiteOr(m.high, 1),
                invert: Boolean(m.invert),
              }
            : {
                mode: "schmitt",
                threshold: finiteOr(m.threshold, 0),
                hysteresis: Math.max(0, finiteOr(m.hysteresis, 0.25)),
                invert: Boolean(m.invert),
              },
      };
      break;
    }
    case "to_state_machine": {
      const m = op.mode;
      normOp = {
        kind: "to_state_machine",
        output_path: op.output_path.trim(),
        mode:
          m.mode === "bit_combine"
            ? {
                mode: "bit_combine",
                bit_channels: m.bit_channels.map((c) => c.trim()),
                threshold: finiteOr(m.threshold, 0.5),
                labels: m.labels.map((l) => ({
                  code: Math.trunc(l.code),
                  name: l.name,
                  color_rgba: l.color_rgba ?? null,
                })),
              }
            : {
                mode: "range_map",
                input_channel: m.input_channel.trim(),
                hysteresis: Math.max(0, finiteOr(m.hysteresis, 0)),
                rules: m.rules.map((r) => ({
                  min: optionalFinite(r.min),
                  max: optionalFinite(r.max),
                  code: Math.trunc(r.code),
                  name: r.name,
                  color_rgba: r.color_rgba ?? null,
                })),
                default_state: {
                  code: Math.trunc(m.default_state.code),
                  name: m.default_state.name,
                  color_rgba: m.default_state.color_rgba ?? null,
                },
              },
      };
      break;
    }
    case "to_analog": {
      const m = op.mode;
      normOp = {
        kind: "to_analog",
        output_path: op.output_path.trim(),
        unit: op.unit?.trim() || null,
        mode:
          m.mode === "bit_weighted_dac"
            ? {
                mode: "bit_weighted_dac",
                bit_channels: m.bit_channels.map((c) => c.trim()),
                scale: finiteOr(m.scale, 1),
                offset: finiteOr(m.offset, 0),
                threshold: finiteOr(m.threshold, 0.5),
              }
            : m.mode === "state_value_map"
              ? {
                  mode: "state_value_map",
                  input_channel: m.input_channel.trim(),
                  entries: m.entries.map((e) => ({
                    code: Math.trunc(e.code),
                    value: finiteOr(e.value, 0),
                  })),
                  default_value: finiteOr(m.default_value, 0),
                }
              : {
                  mode: "level_map",
                  input_channel: m.input_channel.trim(),
                  low_value: finiteOr(m.low_value, 0),
                  high_value: finiteOr(m.high_value, 5),
                  threshold: finiteOr(m.threshold, 0.5),
                  smoothing_tau_ms: Math.max(0, finiteOr(m.smoothing_tau_ms, 0)),
                },
      };
      break;
    }
    case "formula":
      normOp = {
        kind: "formula",
        expression: op.expression,
        output_path: op.output_path.trim(),
        unit: op.unit?.trim() || null,
        output_digital: Boolean(op.output_digital),
      };
      break;
    case "window_stats":
      normOp = {
        kind: "window_stats",
        input_channel: op.input_channel.trim(),
        output_path: op.output_path.trim(),
        unit: op.unit?.trim() || null,
        metric: op.metric,
        window_ms: finiteOr(op.window_ms, 20),
      };
      break;
    case "calculus": {
      const m = op.mode;
      normOp = {
        kind: "calculus",
        input_channel: op.input_channel.trim(),
        output_path: op.output_path.trim(),
        unit: op.unit?.trim() || null,
        mode:
          m.mode === "integral"
            ? {
                mode: "integral",
                scale: finiteOr(m.scale, 1),
                dc_offset: finiteOr(m.dc_offset, 0),
                initial_value: finiteOr(m.initial_value, 0),
                clamp_min: optionalFinite(m.clamp_min),
                clamp_max: optionalFinite(m.clamp_max),
              }
            : {
                mode: "derivative",
                scale: finiteOr(m.scale, 1),
                filter_tau_ms: Math.max(0, finiteOr(m.filter_tau_ms, 0)),
              },
      };
      break;
    }
    case "single_phase_power":
      normOp = {
        kind: "single_phase_power",
        voltage_channel: op.voltage_channel.trim(),
        current_channel: op.current_channel.trim(),
        output_prefix: op.output_prefix.trim(),
        voltage_scale: finiteOr(op.voltage_scale, 1),
        voltage_offset: finiteOr(op.voltage_offset, 0),
        current_scale: finiteOr(op.current_scale, 1),
        current_offset: finiteOr(op.current_offset, 0),
        nominal_freq_hz: finiteOr(op.nominal_freq_hz, 50),
        zero_cross_hysteresis_v: Math.max(0, finiteOr(op.zero_cross_hysteresis_v, 0)),
        min_freq_hz: finiteOr(op.min_freq_hz, 5),
        fallback_window_ms: finiteOr(op.fallback_window_ms, 100),
        outputs: normalizePowerFlags(op.outputs),
      };
      break;
  }

  return {
    node_id: cfg.node_id.trim(),
    name: cfg.name,
    enabled: Boolean(cfg.enabled),
    operator: normOp,
  };
}

// ---------------------------------------------------------------------------
// Formula Compiler & Evaluator (matches Rust ddscope-core::dag::formula)
// ---------------------------------------------------------------------------

type FToken =
  | { type: "num"; value: number }
  | { type: "ch"; path: string }
  | { type: "ident"; name: string }
  | { type: "op"; op: string }
  | { type: "lparen" }
  | { type: "rparen" }
  | { type: "comma" }
  | { type: "q" }
  | { type: "colon" };

function isIdentStartCode(c: number): boolean {
  return (c >= 65 && c <= 90) || (c >= 97 && c <= 122) || c === 95;
}
function isIdentContCode(c: number): boolean {
  return isIdentStartCode(c) || (c >= 48 && c <= 57);
}
function isSegContCode(c: number): boolean {
  return isIdentContCode(c) || c === 46;
}

function tokenizeFormula(input: string): FToken[] {
  const tokens: FToken[] = [];
  let i = 0;
  while (i < input.length) {
    const ch = input[i];
    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }
    if (ch === "[") {
      const end = input.indexOf("]", i + 1);
      if (end < 0) throw new Error("Unclosed '[' in formula channel reference");
      const inner = input.slice(i + 1, end).trim();
      if (!inner || !inner.includes("/")) {
        throw new Error(`Bracketed channel reference '[${inner}]' must be '<source>/<path>'`);
      }
      tokens.push({ type: "ch", path: inner });
      i = end + 1;
      continue;
    }
    const code = input.charCodeAt(i);
    if (
      (code >= 48 && code <= 57) ||
      (ch === "." && i + 1 < input.length && input.charCodeAt(i + 1) >= 48 && input.charCodeAt(i + 1) <= 57)
    ) {
      const rest = input.slice(i);
      const hexMatch = /^0[xX][0-9a-fA-F]+/.exec(rest);
      if (hexMatch) {
        tokens.push({ type: "num", value: Number.parseInt(hexMatch[0], 16) });
        i += hexMatch[0].length;
        continue;
      }
      const numMatch = /^(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?/.exec(rest);
      if (!numMatch) throw new Error(`Invalid numeric literal near '${rest.slice(0, 12)}'`);
      tokens.push({ type: "num", value: Number.parseFloat(numMatch[0]) });
      i += numMatch[0].length;
      continue;
    }
    if (isIdentStartCode(code)) {
      const start = i;
      i += 1;
      while (i < input.length && isIdentContCode(input.charCodeAt(i))) i += 1;
      const ident = input.slice(start, i);
      let look = i;
      while (look < input.length && /\s/.test(input[look])) look += 1;
      const followedByParen = look < input.length && input[look] === "(";
      const isReserved = ident === "t" || ident === "pi" || ident === "e" || ident === "tau";
      if (
        !isReserved &&
        !followedByParen &&
        i + 1 < input.length &&
        input[i] === "/" &&
        isIdentStartCode(input.charCodeAt(i + 1))
      ) {
        while (
          i + 1 < input.length &&
          input[i] === "/" &&
          isIdentStartCode(input.charCodeAt(i + 1))
        ) {
          i += 2;
          while (i < input.length && isSegContCode(input.charCodeAt(i))) i += 1;
        }
        tokens.push({ type: "ch", path: input.slice(start, i) });
        continue;
      }
      tokens.push({ type: "ident", name: ident });
      continue;
    }
    const two = input.slice(i, i + 2);
    if (["==", "!=", "<=", ">=", "&&", "||"].includes(two)) {
      tokens.push({ type: "op", op: two });
      i += 2;
      continue;
    }
    if ("+-*/%^!<>".includes(ch)) {
      tokens.push({ type: "op", op: ch });
      i += 1;
      continue;
    }
    if (ch === "(") {
      tokens.push({ type: "lparen" });
      i += 1;
      continue;
    }
    if (ch === ")") {
      tokens.push({ type: "rparen" });
      i += 1;
      continue;
    }
    if (ch === ",") {
      tokens.push({ type: "comma" });
      i += 1;
      continue;
    }
    if (ch === "?") {
      tokens.push({ type: "q" });
      i += 1;
      continue;
    }
    if (ch === ":") {
      tokens.push({ type: "colon" });
      i += 1;
      continue;
    }
    throw new Error(`Unexpected character '${ch}' in formula`);
  }
  return tokens;
}

type EvalFn = (tSec: number, chVals: number[]) => number;

const FN_ARITIES: Record<string, number> = {
  abs: 1,
  sqrt: 1,
  sin: 1,
  cos: 1,
  tan: 1,
  asin: 1,
  acos: 1,
  atan: 1,
  atan2: 2,
  hypot: 2,
  min: 2,
  max: 2,
  clamp: 3,
  ln: 1,
  log10: 1,
  exp: 1,
  floor: 1,
  ceil: 1,
  round: 1,
  sign: 1,
  deg: 1,
  rad: 1,
};

function sanitize(v: number): number {
  return Number.isFinite(v) ? v : 0;
}

function evalBuiltin(name: string, a: number[]): number {
  switch (name) {
    case "abs":
      return Math.abs(a[0]);
    case "sqrt":
      return a[0] < 0 ? 0 : Math.sqrt(a[0]);
    case "sin":
      return Math.sin(a[0]);
    case "cos":
      return Math.cos(a[0]);
    case "tan":
      return Math.tan(a[0]);
    case "asin":
      return Math.asin(Math.max(-1, Math.min(1, a[0])));
    case "acos":
      return Math.acos(Math.max(-1, Math.min(1, a[0])));
    case "atan":
      return Math.atan(a[0]);
    case "atan2":
      return Math.atan2(a[0], a[1]);
    case "hypot":
      return Math.hypot(a[0], a[1]);
    case "min":
      return Math.min(a[0], a[1]);
    case "max":
      return Math.max(a[0], a[1]);
    case "clamp": {
      const lo = Math.min(a[1], a[2]);
      const hi = Math.max(a[1], a[2]);
      return Math.max(lo, Math.min(hi, a[0]));
    }
    case "ln":
      return a[0] <= 0 ? 0 : Math.log(a[0]);
    case "log10":
      return a[0] <= 0 ? 0 : Math.log10(a[0]);
    case "exp":
      return Math.exp(a[0]);
    case "floor":
      return Math.floor(a[0]);
    case "ceil":
      return Math.ceil(a[0]);
    case "round":
      return Math.round(a[0]);
    case "sign":
      return a[0] > 0 ? 1 : a[0] < 0 ? -1 : 0;
    case "deg":
      return (a[0] * 180) / Math.PI;
    case "rad":
      return (a[0] * Math.PI) / 180;
    default:
      return 0;
  }
}

export function compileVirtualFormula(expression: string): CompiledTsFormula {
  const trimmed = expression.trim();
  if (!trimmed) throw new Error("Formula expression must not be empty");
  const tokens = tokenizeFormula(trimmed);
  let pos = 0;
  const channels: string[] = [];
  const intern = (p: string): number => {
    const idx = channels.indexOf(p);
    if (idx >= 0) return idx;
    channels.push(p);
    return channels.length - 1;
  };

  const peek = () => tokens[pos];
  const parseExpr = (): EvalFn => parseTernary();

  const parseTernary = (): EvalFn => {
    const c = parseOr();
    if (peek()?.type === "q") {
      pos += 1;
      const t = parseExpr();
      if (peek()?.type !== "colon") throw new Error("Expected ':' in ternary expression");
      pos += 1;
      const e = parseTernary();
      return (tSec, chVals) => (c(tSec, chVals) !== 0 ? t(tSec, chVals) : e(tSec, chVals));
    }
    return c;
  };

  const parseOr = (): EvalFn => {
    let lhs = parseAnd();
    while (peek()?.type === "op" && (peek() as { op: string }).op === "||") {
      pos += 1;
      const rhs = parseAnd();
      const l = lhs;
      lhs = (t, c) => (l(t, c) !== 0 || rhs(t, c) !== 0 ? 1 : 0);
    }
    return lhs;
  };

  const parseAnd = (): EvalFn => {
    let lhs = parseEq();
    while (peek()?.type === "op" && (peek() as { op: string }).op === "&&") {
      pos += 1;
      const rhs = parseEq();
      const l = lhs;
      lhs = (t, c) => (l(t, c) !== 0 && rhs(t, c) !== 0 ? 1 : 0);
    }
    return lhs;
  };

  const parseEq = (): EvalFn => {
    let lhs = parseCmp();
    while (peek()?.type === "op" && ["==", "!="].includes((peek() as { op: string }).op)) {
      const op = (peek() as { op: string }).op;
      pos += 1;
      const rhs = parseCmp();
      const l = lhs;
      lhs =
        op === "=="
          ? (t, c) => (Math.abs(l(t, c) - rhs(t, c)) <= 1e-12 ? 1 : 0)
          : (t, c) => (Math.abs(l(t, c) - rhs(t, c)) > 1e-12 ? 1 : 0);
    }
    return lhs;
  };

  const parseCmp = (): EvalFn => {
    let lhs = parseAdd();
    while (
      peek()?.type === "op" &&
      ["<", "<=", ">", ">="].includes((peek() as { op: string }).op)
    ) {
      const op = (peek() as { op: string }).op;
      pos += 1;
      const rhs = parseAdd();
      const l = lhs;
      lhs = (t, c) => {
        const a = l(t, c);
        const b = rhs(t, c);
        if (op === "<") return a < b ? 1 : 0;
        if (op === "<=") return a <= b ? 1 : 0;
        if (op === ">") return a > b ? 1 : 0;
        return a >= b ? 1 : 0;
      };
    }
    return lhs;
  };

  const parseAdd = (): EvalFn => {
    let lhs = parseMul();
    while (peek()?.type === "op" && ["+", "-"].includes((peek() as { op: string }).op)) {
      const op = (peek() as { op: string }).op;
      pos += 1;
      const rhs = parseMul();
      const l = lhs;
      lhs =
        op === "+"
          ? (t, c) => sanitize(l(t, c) + rhs(t, c))
          : (t, c) => sanitize(l(t, c) - rhs(t, c));
    }
    return lhs;
  };

  const parseMul = (): EvalFn => {
    let lhs = parsePow();
    while (peek()?.type === "op" && ["*", "/", "%"].includes((peek() as { op: string }).op)) {
      const op = (peek() as { op: string }).op;
      pos += 1;
      const rhs = parsePow();
      const l = lhs;
      lhs = (t, c) => {
        const a = l(t, c);
        const b = rhs(t, c);
        if (op === "*") return sanitize(a * b);
        if (op === "/") return b === 0 ? 0 : sanitize(a / b);
        return b === 0 ? 0 : sanitize(a % b);
      };
    }
    return lhs;
  };

  const parsePow = (): EvalFn => {
    const lhs = parseUnary();
    if (peek()?.type === "op" && (peek() as { op: string }).op === "^") {
      pos += 1;
      const rhs = parsePow();
      return (t, c) => sanitize(lhs(t, c) ** rhs(t, c));
    }
    return lhs;
  };

  const parseUnary = (): EvalFn => {
    const p = peek();
    if (p?.type === "op" && (p.op === "+" || p.op === "-" || p.op === "!")) {
      pos += 1;
      const inner = parseUnary();
      if (p.op === "+") return inner;
      if (p.op === "-") return (t, c) => sanitize(-inner(t, c));
      return (t, c) => (inner(t, c) === 0 ? 1 : 0);
    }
    return parsePrimary();
  };

  const parsePrimary = (): EvalFn => {
    const tok = tokens[pos];
    if (!tok) throw new Error("Unexpected end of formula");
    pos += 1;
    if (tok.type === "num") {
      const v = tok.value;
      return () => v;
    }
    if (tok.type === "ch") {
      const idx = intern(tok.path);
      return (_t, chVals) => sanitize(chVals[idx] ?? 0);
    }
    if (tok.type === "ident") {
      if (peek()?.type === "lparen") {
        pos += 1;
        const args: EvalFn[] = [];
        if (peek()?.type !== "rparen") {
          while (true) {
            args.push(parseExpr());
            if (peek()?.type === "comma") pos += 1;
            else break;
          }
        }
        if (peek()?.type !== "rparen") {
          throw new Error(`Expected ')' after arguments of '${tok.name}(...)'`);
        }
        pos += 1;
        const expected = FN_ARITIES[tok.name];
        if (expected === undefined) throw new Error(`Unknown formula function '${tok.name}'`);
        if (args.length !== expected) {
          throw new Error(
            `Function '${tok.name}' expects ${expected} argument(s), got ${args.length}`,
          );
        }
        const fnName = tok.name;
        return (t, c) => sanitize(evalBuiltin(fnName, args.map((fn) => fn(t, c))));
      }
      if (tok.name === "t") return (tSec) => tSec;
      if (tok.name === "pi") return () => Math.PI;
      if (tok.name === "tau") return () => Math.PI * 2;
      if (tok.name === "e") return () => Math.E;
      throw new Error(`Unknown identifier '${tok.name}' (use '<source>/<channel>')`);
    }
    if (tok.type === "lparen") {
      const e = parseExpr();
      if (peek()?.type !== "rparen") throw new Error("Expected matching ')' in formula");
      pos += 1;
      return e;
    }
    throw new Error(`Unexpected token in formula`);
  };

  const evalAt = parseExpr();
  if (pos < tokens.length) {
    throw new Error("Unexpected trailing tokens in formula");
  }
  return { inputChannels: channels, evalAt };
}

// ---------------------------------------------------------------------------
// Validation, Expansion & Topological Sort
// ---------------------------------------------------------------------------

export function validateVirtualNodeConfig(cfg: VirtualNodeConfigDto_Serialize): void {
  if (!cfg.node_id.trim()) throw new Error("Virtual node_id must not be empty");
  const op = cfg.operator;
  switch (op.kind) {
    case "analog_to_digital":
      ensureInputPath(op.input_channel, "input_channel");
      ensureLocalSegment(op.output_path, "output_path");
      if (op.mode.mode === "window" && finiteOr(op.mode.low, 0) > finiteOr(op.mode.high, 0)) {
        throw new Error("Window low must be <= high");
      }
      break;
    case "to_state_machine":
      ensureLocalSegment(op.output_path, "output_path");
      if (op.mode.mode === "range_map") {
        ensureInputPath(op.mode.input_channel, "input_channel");
        if (!op.mode.default_state.name.trim()) throw new Error("default_state.name must not be empty");
        for (const r of op.mode.rules) {
          if (!r.name.trim()) throw new Error("Rule name must not be empty");
          if (r.min != null && r.max != null && r.min > r.max) {
            throw new Error(`Rule '${r.name}' min (${r.min}) must be <= max (${r.max})`);
          }
        }
      } else {
        if (op.mode.bit_channels.length === 0 || op.mode.bit_channels.length > 8) {
          throw new Error("BitCombine requires 1..=8 bit_channels");
        }
        op.mode.bit_channels.forEach((ch, i) => ensureInputPath(ch, `bit_channels[${i}]`));
      }
      break;
    case "to_analog":
      ensureLocalSegment(op.output_path, "output_path");
      if (op.mode.mode === "bit_weighted_dac") {
        if (op.mode.bit_channels.length === 0 || op.mode.bit_channels.length > 16) {
          throw new Error("BitWeightedDac requires 1..=16 bit_channels");
        }
        op.mode.bit_channels.forEach((ch, i) => ensureInputPath(ch, `bit_channels[${i}]`));
      } else {
        ensureInputPath(op.mode.input_channel, "input_channel");
      }
      break;
    case "formula":
      ensureLocalSegment(op.output_path, "output_path");
      compileVirtualFormula(op.expression);
      break;
    case "window_stats":
      ensureInputPath(op.input_channel, "input_channel");
      ensureLocalSegment(op.output_path, "output_path");
      if (finiteOr(op.window_ms, 0) <= 0) throw new Error("window_ms must be > 0");
      break;
    case "calculus":
      ensureInputPath(op.input_channel, "input_channel");
      ensureLocalSegment(op.output_path, "output_path");
      if (
        op.mode.mode === "integral" &&
        op.mode.clamp_min != null &&
        op.mode.clamp_max != null &&
        op.mode.clamp_min > op.mode.clamp_max
      ) {
        throw new Error("clamp_min must be <= clamp_max");
      }
      break;
    case "single_phase_power": {
      ensureInputPath(op.voltage_channel, "voltage_channel");
      ensureInputPath(op.current_channel, "current_channel");
      ensureLocalSegment(op.output_prefix, "output_prefix");
      if (finiteOr(op.nominal_freq_hz, 0) <= 0) throw new Error("nominal_freq_hz must be > 0");
      if (finiteOr(op.min_freq_hz, 0) <= 0) throw new Error("min_freq_hz must be > 0");
      if (finiteOr(op.fallback_window_ms, 0) <= 0) {
        throw new Error("fallback_window_ms must be > 0");
      }
      const f = op.outputs;
      const anyOut =
        f.freq ||
        f.u_rms ||
        f.i_rms ||
        f.p ||
        f.q1 ||
        f.q_tot ||
        f.s ||
        f.pf ||
        f.phi_deg ||
        f.u_pk ||
        f.i_pk ||
        f.energy_wh;
      if (!anyOut) throw new Error("SinglePhasePower must enable at least one output");
      break;
    }
  }
}

export function extractVirtualNodeInputPaths(cfg: VirtualNodeConfigDto_Serialize): string[] {
  const out: string[] = [];
  const push = (p: string) => {
    if (!out.includes(p)) out.push(p);
  };
  const op = cfg.operator;
  switch (op.kind) {
    case "analog_to_digital":
    case "window_stats":
    case "calculus":
      push(op.input_channel);
      break;
    case "to_state_machine":
      if (op.mode.mode === "range_map") push(op.mode.input_channel);
      else op.mode.bit_channels.forEach(push);
      break;
    case "to_analog":
      if (op.mode.mode === "bit_weighted_dac") op.mode.bit_channels.forEach(push);
      else push(op.mode.input_channel);
      break;
    case "formula":
      try {
        compileVirtualFormula(op.expression).inputChannels.forEach(push);
      } catch {
        // ignore invalid expression here
      }
      break;
    case "single_phase_power":
      push(op.voltage_channel);
      push(op.current_channel);
      break;
  }
  return out;
}

const POWER_METRIC_META: ReadonlyArray<{
  key: PowerMetricKey;
  titleSuffix: string;
  unit: string | null;
}> = [
  { key: "freq", titleSuffix: "Freq", unit: "Hz" },
  { key: "u_rms", titleSuffix: "Urms", unit: "V" },
  { key: "i_rms", titleSuffix: "Irms", unit: "A" },
  { key: "p", titleSuffix: "Active P", unit: "W" },
  { key: "q1", titleSuffix: "Fund. Q1", unit: "var" },
  { key: "q_tot", titleSuffix: "Total Q", unit: "var" },
  { key: "s", titleSuffix: "Apparent S", unit: "VA" },
  { key: "pf", titleSuffix: "PF", unit: null },
  { key: "phi_deg", titleSuffix: "Phase φ", unit: "deg" },
  { key: "u_pk", titleSuffix: "Upk", unit: "V" },
  { key: "i_pk", titleSuffix: "Ipk", unit: "A" },
  { key: "energy_wh", titleSuffix: "Energy", unit: "Wh" },
];

export function expandVirtualOutputSpecs(
  cfg: VirtualNodeConfigDto_Serialize,
): VirtualOutputSpec[] {
  const prefixName = cfg.name.trim() || cfg.node_id;
  const op = cfg.operator;

  switch (op.kind) {
    case "analog_to_digital":
      return [
        {
          localPath: op.output_path,
          fullPath: `${VIRTUAL_SOURCE_ID}/${op.output_path}`,
          displayName: prefixName,
          unit: null,
          modality: "digital_bool",
          scalarType: "bool",
          stateLabels: [],
        },
      ];
    case "to_state_machine": {
      const labelMap = new Map<number, StateLabelEntryDto_Serialize>();
      if (op.mode.mode === "range_map") {
        labelMap.set(op.mode.default_state.code, {
          code: op.mode.default_state.code,
          name: op.mode.default_state.name,
          color_rgba: op.mode.default_state.color_rgba ?? null,
        });
        for (const r of op.mode.rules) {
          labelMap.set(r.code, {
            code: r.code,
            name: r.name,
            color_rgba: r.color_rgba ?? null,
          });
        }
      } else {
        for (const l of op.mode.labels) {
          labelMap.set(l.code, {
            code: l.code,
            name: l.name,
            color_rgba: l.color_rgba ?? null,
          });
        }
        if (labelMap.size === 0 && op.mode.bit_channels.length <= 4) {
          const w = op.mode.bit_channels.length;
          const cnt = 1 << w;
          for (let code = 0; code < cnt; code += 1) {
            labelMap.set(code, {
              code,
              name: `0b${code.toString(2).padStart(w, "0")}`,
              color_rgba: null,
            });
          }
        }
      }
      const stateLabels = Array.from(labelMap.values()).sort((a, b) => a.code - b.code);
      return [
        {
          localPath: op.output_path,
          fullPath: `${VIRTUAL_SOURCE_ID}/${op.output_path}`,
          displayName: prefixName,
          unit: null,
          modality: "state_machine",
          scalarType: "i32",
          stateLabels,
        },
      ];
    }
    case "to_analog":
    case "window_stats":
    case "calculus":
      return [
        {
          localPath: op.output_path,
          fullPath: `${VIRTUAL_SOURCE_ID}/${op.output_path}`,
          displayName: prefixName,
          unit: op.unit ?? null,
          modality: "scalar",
          scalarType: "f32",
          stateLabels: [],
        },
      ];
    case "formula":
      return [
        {
          localPath: op.output_path,
          fullPath: `${VIRTUAL_SOURCE_ID}/${op.output_path}`,
          displayName: prefixName,
          unit: op.output_digital ? null : (op.unit ?? null),
          modality: op.output_digital ? "digital_bool" : "scalar",
          scalarType: op.output_digital ? "bool" : "f32",
          stateLabels: [],
        },
      ];
    case "single_phase_power": {
      const specs: VirtualOutputSpec[] = [];
      for (const m of POWER_METRIC_META) {
        if (op.outputs[m.key]) {
          const lp = `${op.output_prefix}.${m.key}`;
          specs.push({
            localPath: lp,
            fullPath: `${VIRTUAL_SOURCE_ID}/${lp}`,
            displayName: `${prefixName} ${m.titleSuffix}`,
            unit: m.unit,
            modality: "scalar",
            scalarType: "f32",
            stateLabels: [],
            powerMetricKey: m.key,
          });
        }
      }
      return specs;
    }
  }
}

export function sortVirtualNodesTopologically(
  nodes: VirtualNodeConfigDto_Serialize[],
): VirtualNodeConfigDto_Serialize[] {
  const producerByPath = new Map<string, string>();
  const byId = new Map<string, VirtualNodeConfigDto_Serialize>();
  for (const n of nodes) {
    validateVirtualNodeConfig(n);
    byId.set(n.node_id, n);
    for (const out of expandVirtualOutputSpecs(n)) {
      const existing = producerByPath.get(out.fullPath);
      if (existing && existing !== n.node_id) {
        throw new Error(
          `Virtual output path '${out.fullPath}' conflicts between '${existing}' and '${n.node_id}'`,
        );
      }
      producerByPath.set(out.fullPath, n.node_id);
    }
  }

  const inDeg = new Map<string, number>();
  const edges = new Map<string, string[]>();
  for (const n of nodes) {
    inDeg.set(n.node_id, 0);
    edges.set(n.node_id, []);
  }

  for (const n of nodes) {
    const seenDeps = new Set<string>();
    for (const inp of extractVirtualNodeInputPaths(n)) {
      const prod = producerByPath.get(inp);
      if (!prod) continue;
      if (prod === n.node_id) {
        throw new Error(`Virtual node '${n.node_id}' cannot depend on its own output '${inp}'`);
      }
      if (!seenDeps.has(prod)) {
        seenDeps.add(prod);
        edges.get(prod)!.push(n.node_id);
        inDeg.set(n.node_id, (inDeg.get(n.node_id) ?? 0) + 1);
      }
    }
  }

  const ready = nodes
    .filter((n) => (inDeg.get(n.node_id) ?? 0) === 0)
    .map((n) => n.node_id)
    .sort();
  const order: VirtualNodeConfigDto_Serialize[] = [];
  while (ready.length > 0) {
    const id = ready.shift()!;
    order.push(byId.get(id)!);
    for (const child of edges.get(id) ?? []) {
      const d = (inDeg.get(child) ?? 1) - 1;
      inDeg.set(child, d);
      if (d === 0) {
        ready.push(child);
        ready.sort();
      }
    }
  }

  if (order.length !== nodes.length) {
    throw new Error("Cycle detected in virtual channel DAG");
  }
  return order;
}

// ---------------------------------------------------------------------------
// Single-Phase IEEE 1459 Power Tracker (Mock Simulation Parity)
// ---------------------------------------------------------------------------

export interface PowerSamplePoint {
  tsNs: number;
  freq: number;
  u_rms: number;
  i_rms: number;
  p: number;
  q1: number;
  q_tot: number;
  s: number;
  pf: number;
  phi_deg: number;
  u_pk: number;
  i_pk: number;
  energy_wh: number;
}

interface PowerBin {
  dtSec: number;
  uDt: number;
  iDt: number;
}

class PowerCycleAcc {
  durationSec = 0;
  u2Dt = 0;
  i2Dt = 0;
  pDt = 0;
  uPk = 0;
  iPk = 0;
  bins: PowerBin[] = [];

  reset(): void {
    this.durationSec = 0;
    this.u2Dt = 0;
    this.i2Dt = 0;
    this.pDt = 0;
    this.uPk = 0;
    this.iPk = 0;
    this.bins.length = 0;
  }

  pushSegment(u0: number, u1: number, i0: number, i1: number, dtSec: number, withBins: boolean): void {
    if (dtSec <= 0 || !Number.isFinite(dtSec)) return;
    this.durationSec += dtSec;
    this.u2Dt += 0.5 * (u0 * u0 + u1 * u1) * dtSec;
    this.i2Dt += 0.5 * (i0 * i0 + i1 * i1) * dtSec;
    this.pDt += 0.5 * (u0 * i0 + u1 * i1) * dtSec;
    this.uPk = Math.max(this.uPk, Math.abs(u0), Math.abs(u1));
    this.iPk = Math.max(this.iPk, Math.abs(i0), Math.abs(i1));
    if (withBins) {
      this.bins.push({
        dtSec,
        uDt: 0.5 * (u0 + u1) * dtSec,
        iDt: 0.5 * (i0 + i1) * dtSec,
      });
      if (this.bins.length >= 1024) {
        const merged: PowerBin[] = [];
        for (let idx = 0; idx < this.bins.length; idx += 2) {
          const a = this.bins[idx];
          const b = this.bins[idx + 1];
          merged.push(
            b
              ? { dtSec: a.dtSec + b.dtSec, uDt: a.uDt + b.uDt, iDt: a.iDt + b.iDt }
              : a,
          );
        }
        this.bins = merged;
      }
    }
  }

  finalizeAc(freqHz: number, energyWh: number, tsNs: number): PowerSamplePoint {
    const tTotal = Math.max(this.durationSec, 1e-12);
    const uRms = Math.sqrt(Math.max(0, this.u2Dt / tTotal));
    const iRms = Math.sqrt(Math.max(0, this.i2Dt / tTotal));
    const p = this.pDt / tTotal;
    const s = uRms * iRms;
    const qTot = Math.sqrt(Math.max(0, s * s - p * p));
    const pf = s > 1e-12 ? Math.max(-1, Math.min(1, p / s)) : 0;

    const omega1 = (Math.PI * 2) / tTotal;
    let tau = 0;
    let u1c = 0;
    let u1s = 0;
    let i1c = 0;
    let i1s = 0;
    for (let k = 0; k < this.bins.length; k += 1) {
      const b = this.bins[k];
      const tauMid = tau + 0.5 * b.dtSec;
      tau += b.dtSec;
      const cosW = Math.cos(omega1 * tauMid);
      const sinW = Math.sin(omega1 * tauMid);
      u1c += b.uDt * cosW;
      u1s += b.uDt * sinW;
      i1c += b.iDt * cosW;
      i1s += b.iDt * sinW;
    }
    const scale = 2 / tTotal;
    u1c *= scale;
    u1s *= scale;
    i1c *= scale;
    i1s *= scale;
    const p1 = 0.5 * (u1c * i1c + u1s * i1s);
    const q1 = 0.5 * (u1c * i1s - u1s * i1c);
    const mag = Math.hypot(u1c, u1s) * Math.hypot(i1c, i1s);
    const phiDeg = mag > 1e-12 ? (Math.atan2(q1, p1) * 180) / Math.PI : 0;

    return {
      tsNs,
      freq: freqHz,
      u_rms: uRms,
      i_rms: iRms,
      p,
      q1,
      q_tot: qTot,
      s,
      pf,
      phi_deg: phiDeg,
      u_pk: this.uPk,
      i_pk: this.iPk,
      energy_wh: energyWh,
    };
  }

  finalizeDc(energyWh: number, tsNs: number): PowerSamplePoint {
    const tTotal = Math.max(this.durationSec, 1e-12);
    const uRms = Math.sqrt(Math.max(0, this.u2Dt / tTotal));
    const iRms = Math.sqrt(Math.max(0, this.i2Dt / tTotal));
    const p = this.pDt / tTotal;
    const s = uRms * iRms;
    const qTot = Math.sqrt(Math.max(0, s * s - p * p));
    const pf = s > 1e-12 ? Math.max(-1, Math.min(1, p / s)) : 0;
    return {
      tsNs,
      freq: 0,
      u_rms: uRms,
      i_rms: iRms,
      p,
      q1: 0,
      q_tot: qTot,
      s,
      pf,
      phi_deg: 0,
      u_pk: this.uPk,
      i_pk: this.iPk,
      energy_wh: energyWh,
    };
  }
}

export class MockSinglePhasePowerTracker {
  private readonly cfg: Extract<VirtualNodeConfigDto_Serialize["operator"], { kind: "single_phase_power" }>;
  private readonly tauFiltSec: number;
  private readonly minPeriodNs: number;
  private readonly maxCycleNs: number;
  private readonly fallbackWindowNs: number;

  private initialized = false;
  private prevTsNs = 0;
  private prevU = 0;
  private prevI = 0;
  private prevUFilt = 0;
  private trackedUPk = 0;
  private armedNeg = false;
  private windowStartNs = 0;
  private lastZcNs: number | null = null;
  private lastAcEmitNs: number | null = null;
  private fallbackStartNs = 0;
  private energyWh = 0;
  private readonly cycleAcc = new PowerCycleAcc();
  private readonly fallbackAcc = new PowerCycleAcc();
  readonly history: PowerSamplePoint[] = [];

  constructor(
    op: Extract<VirtualNodeConfigDto_Serialize["operator"], { kind: "single_phase_power" }>,
  ) {
    this.cfg = op;
    const nom = Math.max(0.1, finiteOr(op.nominal_freq_hz, 50));
    const minF = Math.max(0.01, finiteOr(op.min_freq_hz, 5));
    this.tauFiltSec = 1 / (Math.PI * 2 * 4 * nom);
    this.minPeriodNs = Math.max(1, Math.round(1e9 / Math.max(2000, 20 * nom)));
    this.maxCycleNs = Math.max(1, Math.round(1e9 / minF));
    this.fallbackWindowNs = Math.max(1, Math.round(Math.max(0.1, finiteOr(op.fallback_window_ms, 100)) * 1e6));
  }

  getLastTsNs(): number {
    return this.initialized ? this.prevTsNs : 0;
  }

  stepRange(
    fromNs: number,
    toNs: number,
    evalPair: (tNs: number) => [number, number],
  ): void {
    const stepNs = 50_000; // 20 kHz analytical integration
    if (!this.initialized) {
      const start = Math.max(0, fromNs);
      const [u0, i0] = evalPair(start);
      this.processSample(start, u0, i0);
    }
    let cur = Math.max(this.prevTsNs + stepNs, fromNs);
    const maxSteps = 12_000;
    let steps = 0;
    while (cur <= toNs && steps < maxSteps) {
      const [u, i] = evalPair(cur);
      this.processSample(cur, u, i);
      cur += stepNs;
      steps += 1;
    }
    if (toNs > this.prevTsNs) {
      const [uEnd, iEnd] = evalPair(toNs);
      this.processSample(toNs, uEnd, iEnd);
    }
  }

  private pushHistory(pt: PowerSamplePoint): void {
    this.history.push(pt);
    if (this.history.length > 4096) {
      this.history.splice(0, this.history.length - 4096);
    }
  }

  sampleAt(tNs: number, metric: PowerMetricKey): number {
    if (this.history.length === 0) return 0;
    if (tNs <= this.history[0].tsNs) return this.history[0][metric];
    const last = this.history[this.history.length - 1];
    if (tNs >= last.tsNs) return last[metric];
    let lo = 0;
    let hi = this.history.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (this.history[mid].tsNs <= tNs) lo = mid;
      else hi = mid - 1;
    }
    return this.history[lo][metric];
  }

  processSample(tsNs: number, rawU: number, rawI: number): void {
    const uScale = finiteOr(this.cfg.voltage_scale, 1);
    const uOff = finiteOr(this.cfg.voltage_offset, 0);
    const iScale = finiteOr(this.cfg.current_scale, 1);
    const iOff = finiteOr(this.cfg.current_offset, 0);
    const nom = Math.max(0.1, finiteOr(this.cfg.nominal_freq_hz, 50));
    const minF = Math.max(0.01, finiteOr(this.cfg.min_freq_hz, 5));
    const zcHystCfg = finiteOr(this.cfg.zero_cross_hysteresis_v, 0);

    const u = (Number.isFinite(rawU) ? rawU : 0) * uScale + uOff;
    const i = (Number.isFinite(rawI) ? rawI : 0) * iScale + iOff;

    if (!this.initialized) {
      this.initialized = true;
      this.prevTsNs = tsNs;
      this.prevU = u;
      this.prevI = i;
      this.prevUFilt = u;
      this.trackedUPk = Math.abs(u);
      this.armedNeg = u < 0;
      this.windowStartNs = tsNs;
      this.fallbackStartNs = tsNs;
      return;
    }

    const dtNs = Math.max(0, tsNs - this.prevTsNs);
    if (dtNs === 0) {
      this.prevU = u;
      this.prevI = i;
      return;
    }

    const dtSec = dtNs * 1e-9;
    this.trackedUPk = Math.max(this.trackedUPk * Math.exp(-dtSec * nom * 0.5), Math.abs(u));
    this.energyWh += (0.5 * (this.prevU * this.prevI + u * i) * dtSec) / 3600;

    const alpha = dtSec / (this.tauFiltSec + dtSec);
    const uFilt = this.prevUFilt + alpha * (u - this.prevUFilt);
    const hyst = zcHystCfg > 0 ? zcHystCfg : Math.max(1e-3, 0.05 * this.trackedUPk);

    if (!this.armedNeg && uFilt <= -hyst) {
      this.armedNeg = true;
    }

    let cycleCompleted = false;
    if (this.armedNeg && uFilt >= hyst) {
      const denom = uFilt - this.prevUFilt;
      const frac =
        Math.abs(denom) > 1e-12 ? Math.max(0, Math.min(1, (hyst - this.prevUFilt) / denom)) : 0.5;
      const tZcNs = this.prevTsNs + Math.round(dtNs * frac);

      if (this.lastZcNs === null || tZcNs - this.lastZcNs >= this.minPeriodNs) {
        this.armedNeg = false;
        const uZc = this.prevU + (u - this.prevU) * frac;
        const iZc = this.prevI + (i - this.prevI) * frac;
        const dtFirst = Math.max(0, tZcNs - this.prevTsNs) * 1e-9;
        const dtSecond = Math.max(0, tsNs - tZcNs) * 1e-9;

        this.cycleAcc.pushSegment(this.prevU, uZc, this.prevI, iZc, dtFirst, true);
        this.fallbackAcc.pushSegment(this.prevU, uZc, this.prevI, iZc, dtFirst, false);

        if (this.lastZcNs !== null) {
          const periodNs = tZcNs - this.lastZcNs;
          if (periodNs > 0) {
            const freqHz = 1e9 / periodNs;
            if (freqHz >= minF) {
              const tailEnergy = (0.5 * (uZc * iZc + u * i) * dtSecond) / 3600;
              const sample = this.cycleAcc.finalizeAc(freqHz, this.energyWh - tailEnergy, tZcNs);
              this.lastAcEmitNs = tZcNs;
              this.pushHistory(sample);
              cycleCompleted = true;
            }
          }
        }

        this.lastZcNs = tZcNs;
        this.cycleAcc.reset();
        this.cycleAcc.pushSegment(uZc, u, iZc, i, dtSecond, true);

        if (cycleCompleted) {
          this.fallbackAcc.reset();
          this.fallbackStartNs = tZcNs;
        }
        this.fallbackAcc.pushSegment(uZc, u, iZc, i, dtSecond, false);
      }
    }

    if (!cycleCompleted && (this.lastZcNs === null || this.lastZcNs < this.prevTsNs)) {
      this.cycleAcc.pushSegment(this.prevU, u, this.prevI, i, dtSec, true);
      this.fallbackAcc.pushSegment(this.prevU, u, this.prevI, i, dtSec, false);
    }

    const noRecentAc =
      this.lastAcEmitNs !== null
        ? tsNs - this.lastAcEmitNs >= this.maxCycleNs
        : tsNs - this.windowStartNs >= this.maxCycleNs;

    if (noRecentAc && tsNs - this.fallbackStartNs >= this.fallbackWindowNs) {
      const sample = this.fallbackAcc.finalizeDc(this.energyWh, tsNs);
      this.pushHistory(sample);
      this.fallbackAcc.reset();
      this.fallbackStartNs = tsNs;
    }

    this.prevTsNs = tsNs;
    this.prevU = u;
    this.prevI = i;
    this.prevUFilt = uFilt;
  }
}

export function matchRangeRuleCode(
  val: number,
  rules: ReadonlyArray<{ min: number | null; max: number | null; code: number }>,
  defaultCode: number,
): number {
  for (let i = 0; i < rules.length; i += 1) {
    const r = rules[i];
    if (r.min != null && r.max != null && Math.abs(r.min - r.max) <= 1e-12) {
      if (Math.abs(val - r.min) <= 1e-12) return r.code;
      continue;
    }
    const geMin = r.min == null || val >= r.min;
    const ltMax = r.max == null || val < r.max;
    if (geMin && ltMax) return r.code;
  }
  return defaultCode;
}
