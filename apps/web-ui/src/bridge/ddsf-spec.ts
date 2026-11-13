/**
 * Frozen `DDSF v1` (DDScope Frame v1) binary layout specification mirrored from
 * `crates/ddscope-core/src/scope/ddsf.rs`.
 */

export const DDSF_MAGIC = 0x4653_4444; // Little-endian "DDSF"
export const DDSF_VERSION_V1 = 1;

export const DDSF_HEADER_BYTES = 64;
export const DDSF_SCOPE_HEADER_BYTES = 32;
export const DDSF_TRACK_HEADER_BYTES = 32;
export const DDSF_TRACK_STATS_BYTES = 32;
export const DDSF_SNAPSHOT_ENTRY_BYTES = 16;

export const DDSF_FLAG_QUOTA_WARNING = 1 << 0;
export const DDSF_FLAG_PINNED_ACTIVE = 1 << 1;

export const DDSF_TRACK_FLAG_HAS_STATS = 1 << 0;
export const DDSF_TRACK_FLAG_CLIPPED = 1 << 1;

export const DDSF_SHM_SLOT_COUNT = 3;
export const DDSF_SHM_SLOT_BYTES = 8 * 1024 * 1024;

export const DdsfBlockKind = {
  AnalogRawXY: 1,
  AnalogM4Column: 2,
  DigitalEdges: 3,
  StateRibbons: 4,
  SpectrumFrame: 5,
} as const;
export type DdsfBlockKindCode = (typeof DdsfBlockKind)[keyof typeof DdsfBlockKind];

export const DdsfSnapshotKind = {
  Scalar: 0,
  Bool: 1,
  StateEnum: 2,
} as const;
export type DdsfSnapshotKindCode =
  (typeof DdsfSnapshotKind)[keyof typeof DdsfSnapshotKind];

export const TriggerSweepModeCode = {
  Roll: 0,
  Auto: 1,
  Normal: 2,
  Single: 3,
} as const;

export const TriggerRuntimeStateCode = {
  Roll: 0,
  Armed: 1,
  AutoFreerun: 2,
  Triggered: 3,
  LatchedPinned: 4,
} as const;

export const DdsfHeaderOffsets = {
  magic: 0x00,
  version: 0x04,
  flags: 0x06,
  frameSeq: 0x08,
  scopeCount: 0x0c,
  snapshotCount: 0x0e,
  hostNowNs: 0x10,
  totalByteLen: 0x18,
} as const;

export const DdsfScopeHeaderOffsets = {
  scopeId: 0x00,
  triggerState: 0x04,
  mode: 0x05,
  trackBlockCount: 0x06,
  winTStartNs: 0x08,
  winTSpanNs: 0x10,
  subpixelOffsetNs: 0x18,
  packetByteLen: 0x1c,
} as const;

export const DdsfTrackHeaderOffsets = {
  channelId: 0x00,
  blockKind: 0x04,
  digitalInitLevel: 0x05,
  trackFlags: 0x06,
  elementCount: 0x08,
  dataByteLen: 0x0c,
  blockByteLen: 0x10,
  auxF32_0: 0x14,
  auxF32_1: 0x18,
  reserved: 0x1c,
} as const;

export const DdsfTrackStatsOffsets = {
  min: 0x00,
  max: 0x04,
  vpp: 0x08,
  mean: 0x0c,
  rms: 0x10,
  freqHz: 0x14,
  duty: 0x18,
  reserved: 0x1c,
} as const;

export const DdsfSnapshotOffsets = {
  channelNumId: 0x00,
  kind: 0x04,
  valF64: 0x08,
} as const;

export const DDSF_ANALOG_POINT_FLOATS = 2;
export const DDSF_M4_COLUMN_FLOATS = 5;
export const DDSF_STATE_RIBBON_BYTES = 16;

export function alignUp8(len: number): number {
  return (len + 7) & ~7;
}

export function alignUp64(len: number): number {
  return (len + 63) & ~63;
}
