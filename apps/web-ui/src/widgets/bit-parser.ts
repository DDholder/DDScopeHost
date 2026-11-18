import type {
  BitParserTone,
  BitParserWidth,
  WidgetBitItem,
  WidgetBitParserConfig,
} from "./types.ts";

export const VALID_BIT_WIDTHS: ReadonlyArray<BitParserWidth> = [8, 16, 32];
export const VALID_BIT_TONES: ReadonlyArray<BitParserTone> = [
  "info",
  "ok",
  "warn",
  "fault",
];

function isPlainRecord(val: unknown): val is Record<string, unknown> {
  return typeof val === "object" && val !== null && !Array.isArray(val);
}

/**
 * Create a default 16-bit parser configuration with no labeled bits yet.
 */
export function createDefaultBitParserConfig(): WidgetBitParserConfig {
  return {
    bitWidth: 16,
    bits: [],
    showUnlabeled: false,
  };
}

/**
 * Validate and normalize an untrusted or serialized `WidgetBitParserConfig`.
 */
export function normalizeBitParserConfig(
  raw: unknown,
): WidgetBitParserConfig | undefined {
  if (!isPlainRecord(raw)) return undefined;

  const bitWidth: BitParserWidth =
    raw.bitWidth === 8 || raw.bitWidth === 16 || raw.bitWidth === 32
      ? raw.bitWidth
      : 16;
  const showUnlabeled = Boolean(raw.showUnlabeled);

  const byBit = new Map<number, WidgetBitItem>();
  if (Array.isArray(raw.bits)) {
    for (let i = 0; i < raw.bits.length; i += 1) {
      const itemRaw = raw.bits[i];
      if (!isPlainRecord(itemRaw)) continue;
      const bit = itemRaw.bit;
      if (
        typeof bit !== "number" ||
        !Number.isInteger(bit) ||
        bit < 0 ||
        bit >= 32
      ) {
        continue;
      }
      if (typeof itemRaw.label !== "string" || itemRaw.label.trim().length === 0) {
        continue;
      }
      const tone: BitParserTone =
        typeof itemRaw.tone === "string" &&
        (VALID_BIT_TONES as readonly string[]).includes(itemRaw.tone)
          ? (itemRaw.tone as BitParserTone)
          : "info";
      if (!byBit.has(bit)) {
        byBit.set(bit, {
          bit,
          label: itemRaw.label,
          tone,
        });
      }
    }
  }

  const bits = Array.from(byBit.values()).sort((a, b) => a.bit - b.bit);
  return {
    bitWidth,
    bits,
    showUnlabeled,
  };
}

/**
 * Convert a live floating-point channel sample into an unsigned integer masked
 * to the configured bit width (`8`, `16`, or `32`). Returns `null` for non-finite values.
 */
export function toUnsignedBitWord(
  rawVal: number,
  bitWidth: BitParserWidth,
): number | null {
  if (!Number.isFinite(rawVal)) return null;
  const intVal = Math.round(rawVal);
  const u32 = intVal >>> 0;
  if (bitWidth === 8) return u32 & 0xff;
  if (bitWidth === 16) return u32 & 0xffff;
  return u32;
}

/**
 * Test whether bit `bit` (`0..31`) is `1` in an unsigned 32-bit word.
 */
export function isBitSet(word: number, bit: number): boolean {
  if (!Number.isInteger(bit) || bit < 0 || bit >= 32) return false;
  return ((word >>> bit) & 1) === 1;
}

/**
 * Format an unsigned bit word as zero-padded uppercase hexadecimal (`0x....`).
 */
export function formatBitWordHex(
  word: number,
  bitWidth: BitParserWidth,
): string {
  const digits = bitWidth / 4;
  return `0x${(word >>> 0).toString(16).toUpperCase().padStart(digits, "0")}`;
}

export interface ActiveBitEntry {
  bit: number;
  label: string;
  tone: BitParserTone;
  isFallback: boolean;
}

/**
 * Evaluate all bits in `0 .. config.bitWidth - 1` that are currently `1`
 * and return their display entries in ascending LSB-to-MSB order.
 */
export function evaluateActiveBits(
  word: number,
  config: WidgetBitParserConfig,
): ActiveBitEntry[] {
  const configuredMap = new Map<number, WidgetBitItem>();
  for (let i = 0; i < config.bits.length; i += 1) {
    const item = config.bits[i];
    if (
      Number.isInteger(item.bit) &&
      item.bit >= 0 &&
      item.bit < config.bitWidth &&
      item.label.trim().length > 0
    ) {
      configuredMap.set(item.bit, item);
    }
  }

  // Show raw `Bit N` flags immediately when nothing has been named yet so a
  // newly placed widget still gives live visual feedback before configuration.
  const includeUnlabeled = Boolean(config.showUnlabeled) || configuredMap.size === 0;
  const active: ActiveBitEntry[] = [];

  for (let b = 0; b < config.bitWidth; b += 1) {
    if (!isBitSet(word, b)) continue;
    const item = configuredMap.get(b);
    if (item) {
      active.push({
        bit: b,
        label: item.label,
        tone: item.tone,
        isFallback: false,
      });
    } else if (includeUnlabeled) {
      active.push({
        bit: b,
        label: `Bit ${b}`,
        tone: "info",
        isFallback: true,
      });
    }
  }

  return active;
}

/**
 * Update or insert the configuration entry for a single bit (`0..31`), keeping
 * `bits` sorted ascending and removing entries only when both label and tone are reset.
 */
export function upsertBitConfigItem(
  config: WidgetBitParserConfig,
  bit: number,
  patch: { label?: string; tone?: BitParserTone },
): WidgetBitParserConfig {
  if (!Number.isInteger(bit) || bit < 0 || bit >= 32) {
    return config;
  }

  const existing = config.bits.find((b) => b.bit === bit);
  const nextLabel = patch.label ?? existing?.label ?? "";
  const nextTone: BitParserTone = patch.tone ?? existing?.tone ?? "info";

  const filtered = config.bits.filter((b) => b.bit !== bit);
  if (nextLabel.length === 0 && nextTone === "info") {
    return {
      ...config,
      bits: filtered,
    };
  }

  const nextBits = [
    ...filtered,
    {
      bit,
      label: nextLabel,
      tone: nextTone,
    },
  ].sort((a, b) => a.bit - b.bit);

  return {
    ...config,
    bits: nextBits,
  };
}
