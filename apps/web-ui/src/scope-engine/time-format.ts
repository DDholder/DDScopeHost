export function formatTimeNs(ns: number): string {
  const abs = Math.abs(ns);
  const sign = ns < 0 ? "-" : "";
  if (abs < 1_000) {
    return `${sign}${Math.round(abs)} ns`;
  }
  if (abs < 1_000_000) {
    return `${sign}${(abs / 1_000).toFixed(abs < 10_000 ? 2 : 1)} µs`;
  }
  if (abs < 1_000_000_000) {
    return `${sign}${(abs / 1_000_000).toFixed(abs < 10_000_000 ? 2 : 1)} ms`;
  }
  if (abs < 60_000_000_000) {
    return `${sign}${(abs / 1_000_000_000).toFixed(2)} s`;
  }
  const mins = abs / 60_000_000_000;
  return `${sign}${mins.toFixed(1)} min`;
}

export function formatTimePerDiv(spanNs: number, divisions = 10): string {
  const perDiv = spanNs / Math.max(1, divisions);
  return `${formatTimeNs(perDiv)}/div`;
}

export function formatFrequencyHz(hz: number): string {
  if (!Number.isFinite(hz) || hz <= 0) return "--";
  if (hz >= 1_000_000) return `${(hz / 1_000_000).toFixed(2)} MHz`;
  if (hz >= 1_000) return `${(hz / 1_000).toFixed(2)} kHz`;
  return `${hz.toFixed(1)} Hz`;
}

export function formatMetricValue(v: number, unit?: string | null): string {
  if (!Number.isFinite(v)) return "--";
  const abs = Math.abs(v);
  let numStr: string;
  if (abs >= 1000) numStr = v.toFixed(1);
  else if (abs >= 10) numStr = v.toFixed(2);
  else numStr = v.toFixed(3);
  return unit ? `${numStr} ${unit}` : numStr;
}

export function formatBytesCompact(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${Math.round(bytes)} B`;
}

export function formatSampleRate(sps: number): string {
  if (!Number.isFinite(sps) || sps <= 0) return "0 S/s";
  if (sps >= 1_000_000) return `${(sps / 1_000_000).toFixed(2)} MS/s`;
  if (sps >= 1_000) return `${(sps / 1_000).toFixed(1)} kS/s`;
  return `${Math.round(sps)} S/s`;
}

export function rgbaNumberToFloat4(rgba: number): [number, number, number, number] {
  const r = ((rgba >>> 24) & 0xff) / 255;
  const g = ((rgba >>> 16) & 0xff) / 255;
  const b = ((rgba >>> 8) & 0xff) / 255;
  const a = (rgba & 0xff) / 255;
  return [r, g, b, a > 0 ? a : 1];
}

export function rgbaNumberToCss(rgba: number, alphaOverride?: number): string {
  const r = (rgba >>> 24) & 0xff;
  const g = (rgba >>> 16) & 0xff;
  const b = (rgba >>> 8) & 0xff;
  const a = alphaOverride ?? ((rgba & 0xff) / 255 || 1);
  return `rgba(${r}, ${g}, ${b}, ${a})`;
}
