import {
  DDSF_ANALOG_POINT_FLOATS,
  DDSF_HEADER_BYTES,
  DDSF_M4_COLUMN_FLOATS,
  DDSF_MAGIC,
  DDSF_SCOPE_HEADER_BYTES,
  DDSF_SNAPSHOT_ENTRY_BYTES,
  DDSF_STATE_RIBBON_BYTES,
  DDSF_TRACK_FLAG_HAS_STATS,
  DDSF_TRACK_HEADER_BYTES,
  DDSF_TRACK_STATS_BYTES,
  DDSF_VERSION_V1,
  DdsfBlockKind,
  DdsfHeaderOffsets,
  DdsfScopeHeaderOffsets,
  DdsfSnapshotOffsets,
  DdsfTrackHeaderOffsets,
  DdsfTrackStatsOffsets,
  alignUp8,
  type DdsfBlockKindCode,
  type DdsfSnapshotKindCode,
} from "./ddsf-spec.ts";

export interface DdsfFrameHeaderView {
  magic: number;
  version: number;
  flags: number;
  frameSeq: number;
  scopeCount: number;
  snapshotCount: number;
  hostNowNs: number;
  totalByteLen: number;
}

export interface DdsfTrackStatsView {
  min: number;
  max: number;
  vpp: number;
  mean: number;
  rms: number;
  freqHz: number;
  duty: number;
}

export type DdsfTrackPayloadView =
  | { kind: typeof DdsfBlockKind.AnalogRawXY; points: Float32Array }
  | { kind: typeof DdsfBlockKind.AnalogM4Column; columns: Float32Array }
  | { kind: typeof DdsfBlockKind.DigitalEdges; initLevel: 0 | 1; edges: Float32Array }
  | {
      kind: typeof DdsfBlockKind.StateRibbons;
      /**
       * Interleaved float view where each ribbon occupies 4 floats: `[x0_norm, x1_norm, _, _]`.
       */
      floats: Float32Array;
      /**
       * Interleaved i32 view where each ribbon's `state_id` is at index `i * 4 + 2`.
       */
      stateIds: Int32Array;
      /**
       * Interleaved u32 view where each ribbon's `color_idx` is at index `i * 4 + 3`.
       */
      colorIndices: Uint32Array;
    }
  | { kind: typeof DdsfBlockKind.SpectrumFrame; bins: Float32Array; fStart: number; df: number };

export interface DdsfTrackBlockView {
  channelId: number;
  blockKind: DdsfBlockKindCode;
  digitalInitLevel: 0 | 1;
  trackFlags: number;
  elementCount: number;
  dataByteLen: number;
  blockByteLen: number;
  auxF32_0: number;
  auxF32_1: number;
  stats: DdsfTrackStatsView | null;
  payload: DdsfTrackPayloadView;
}

export interface DdsfScopeView {
  scopeId: number;
  triggerState: number;
  mode: number;
  trackBlockCount: number;
  winTStartNs: number;
  winTSpanNs: number;
  subpixelOffsetNs: number;
  packetByteLen: number;
  tracks: DdsfTrackBlockView[];
}

export interface DdsfSnapshotView {
  channelNumId: number;
  kind: DdsfSnapshotKindCode;
  valF64: number;
}

export interface DdsfFrameView {
  header: DdsfFrameHeaderView;
  scopes: DdsfScopeView[];
  snapshots: DdsfSnapshotView[];
  byteLength: number;
}

export class DdsfParseError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DdsfParseError";
  }
}

function readU64AsNumber(view: DataView, offset: number): number {
  const lo = view.getUint32(offset, true);
  const hi = view.getUint32(offset + 4, true);
  return hi * 4_294_967_296 + lo;
}

function writeU64FromNumber(view: DataView, offset: number, value: number): void {
  const clamped = Number.isFinite(value) && value >= 0 ? Math.floor(value) : 0;
  const hi = Math.floor(clamped / 4_294_967_296);
  const lo = clamped - hi * 4_294_967_296;
  view.setUint32(offset, lo >>> 0, true);
  view.setUint32(offset + 4, hi >>> 0, true);
}

function createFloat32View(
  buffer: ArrayBufferLike,
  absOffset: number,
  floatCount: number,
): Float32Array {
  if (floatCount === 0) {
    return new Float32Array(0);
  }
  if ((absOffset & 3) === 0) {
    return new Float32Array(buffer, absOffset, floatCount);
  }
  const byteLen = floatCount * 4;
  const copy = new Uint8Array(byteLen);
  copy.set(new Uint8Array(buffer, absOffset, byteLen));
  return new Float32Array(copy.buffer);
}

function createStateRibbonViews(
  buffer: ArrayBufferLike,
  absOffset: number,
  elementCount: number,
): { floats: Float32Array; stateIds: Int32Array; colorIndices: Uint32Array } {
  if (elementCount === 0) {
    return {
      floats: new Float32Array(0),
      stateIds: new Int32Array(0),
      colorIndices: new Uint32Array(0),
    };
  }
  const wordCount = elementCount * 4;
  if ((absOffset & 3) === 0) {
    return {
      floats: new Float32Array(buffer, absOffset, wordCount),
      stateIds: new Int32Array(buffer, absOffset, wordCount),
      colorIndices: new Uint32Array(buffer, absOffset, wordCount),
    };
  }
  const byteLen = elementCount * DDSF_STATE_RIBBON_BYTES;
  const copy = new Uint8Array(byteLen);
  copy.set(new Uint8Array(buffer, absOffset, byteLen));
  return {
    floats: new Float32Array(copy.buffer),
    stateIds: new Int32Array(copy.buffer),
    colorIndices: new Uint32Array(copy.buffer),
  };
}

/**
 * Zero-copy parser for `DDSF v1` frames matching `crates/ddscope-core/src/scope/ddsf.rs`.
 */
export function parseDdsfFrame(
  buffer: ArrayBufferLike,
  byteOffset = 0,
  byteLength = buffer.byteLength - byteOffset,
): DdsfFrameView {
  if (byteOffset < 0 || byteLength < 0 || byteOffset + byteLength > buffer.byteLength) {
    throw new DdsfParseError("DDSF buffer slice is out of bounds");
  }
  if (byteLength < DDSF_HEADER_BYTES) {
    throw new DdsfParseError(
      `buffer length (${byteLength}) smaller than DDSF frame header (${DDSF_HEADER_BYTES})`,
    );
  }

  const view = new DataView(buffer, byteOffset, byteLength);
  const magic = view.getUint32(DdsfHeaderOffsets.magic, true);
  if (magic !== DDSF_MAGIC) {
    throw new DdsfParseError(`invalid DDSF magic: 0x${magic.toString(16)}`);
  }

  const version = view.getUint16(DdsfHeaderOffsets.version, true);
  if (version !== DDSF_VERSION_V1) {
    throw new DdsfParseError(`unsupported DDSF version: ${version}`);
  }

  const flags = view.getUint16(DdsfHeaderOffsets.flags, true);
  const frameSeq = view.getUint32(DdsfHeaderOffsets.frameSeq, true);
  const scopeCount = view.getUint16(DdsfHeaderOffsets.scopeCount, true);
  const snapshotCount = view.getUint16(DdsfHeaderOffsets.snapshotCount, true);
  const hostNowNs = readU64AsNumber(view, DdsfHeaderOffsets.hostNowNs);
  const totalByteLen = readU64AsNumber(view, DdsfHeaderOffsets.totalByteLen);

  if (totalByteLen < DDSF_HEADER_BYTES) {
    throw new DdsfParseError(`total_byte_len (${totalByteLen}) smaller than header size`);
  }
  if (totalByteLen > byteLength) {
    throw new DdsfParseError(
      `total_byte_len (${totalByteLen}) exceeds buffer length (${byteLength})`,
    );
  }

  const snapshotsByteLen = snapshotCount * DDSF_SNAPSHOT_ENTRY_BYTES;
  const snapshotsStart = totalByteLen - snapshotsByteLen;
  if (snapshotsStart < DDSF_HEADER_BYTES) {
    throw new DdsfParseError("invalid snapshot entries offset");
  }

  const header: DdsfFrameHeaderView = {
    magic,
    version,
    flags,
    frameSeq,
    scopeCount,
    snapshotCount,
    hostNowNs,
    totalByteLen,
  };

  const scopes: DdsfScopeView[] = new Array(scopeCount);
  let cursor = DDSF_HEADER_BYTES;

  for (let s = 0; s < scopeCount; s += 1) {
    if (cursor + DDSF_SCOPE_HEADER_BYTES > snapshotsStart) {
      throw new DdsfParseError(`truncated scope header at index ${s}`);
    }

    const scopeId = view.getUint32(cursor + DdsfScopeHeaderOffsets.scopeId, true);
    const triggerState = view.getUint8(cursor + DdsfScopeHeaderOffsets.triggerState);
    const mode = view.getUint8(cursor + DdsfScopeHeaderOffsets.mode);
    const trackBlockCount = view.getUint16(cursor + DdsfScopeHeaderOffsets.trackBlockCount, true);
    const winTStartNs = readU64AsNumber(view, cursor + DdsfScopeHeaderOffsets.winTStartNs);
    const winTSpanNs = readU64AsNumber(view, cursor + DdsfScopeHeaderOffsets.winTSpanNs);
    const subpixelOffsetNs = view.getFloat32(
      cursor + DdsfScopeHeaderOffsets.subpixelOffsetNs,
      true,
    );
    const packetByteLen = view.getUint32(cursor + DdsfScopeHeaderOffsets.packetByteLen, true);

    if (packetByteLen < DDSF_SCOPE_HEADER_BYTES || cursor + packetByteLen > snapshotsStart) {
      throw new DdsfParseError(
        `invalid scope packet_byte_len (${packetByteLen}) for scope ${scopeId}`,
      );
    }

    const scopeEnd = cursor + packetByteLen;
    let trackCursor = cursor + DDSF_SCOPE_HEADER_BYTES;
    const tracks: DdsfTrackBlockView[] = new Array(trackBlockCount);

    for (let t = 0; t < trackBlockCount; t += 1) {
      if (trackCursor + DDSF_TRACK_HEADER_BYTES > scopeEnd) {
        throw new DdsfParseError(`truncated track header at scope ${scopeId}, track ${t}`);
      }

      const channelId = view.getUint32(trackCursor + DdsfTrackHeaderOffsets.channelId, true);
      const rawKind = view.getUint8(trackCursor + DdsfTrackHeaderOffsets.blockKind);
      const rawInit = view.getUint8(trackCursor + DdsfTrackHeaderOffsets.digitalInitLevel);
      const digitalInitLevel: 0 | 1 = rawInit ? 1 : 0;
      const trackFlags = view.getUint16(trackCursor + DdsfTrackHeaderOffsets.trackFlags, true);
      const elementCount = view.getUint32(trackCursor + DdsfTrackHeaderOffsets.elementCount, true);
      const dataByteLen = view.getUint32(trackCursor + DdsfTrackHeaderOffsets.dataByteLen, true);
      const blockByteLen = view.getUint32(trackCursor + DdsfTrackHeaderOffsets.blockByteLen, true);
      const auxF32_0 = view.getFloat32(trackCursor + DdsfTrackHeaderOffsets.auxF32_0, true);
      const auxF32_1 = view.getFloat32(trackCursor + DdsfTrackHeaderOffsets.auxF32_1, true);

      if (blockByteLen < DDSF_TRACK_HEADER_BYTES || trackCursor + blockByteLen > scopeEnd) {
        throw new DdsfParseError(`invalid track block_byte_len (${blockByteLen})`);
      }

      const hasStats = (trackFlags & DDSF_TRACK_FLAG_HAS_STATS) !== 0;
      let stats: DdsfTrackStatsView | null = null;
      let payloadRelOffset = DDSF_TRACK_HEADER_BYTES;

      if (hasStats) {
        if (DDSF_TRACK_HEADER_BYTES + DDSF_TRACK_STATS_BYTES > blockByteLen) {
          throw new DdsfParseError("track block flagged HAS_STATS but smaller than 64 bytes");
        }
        const statsBase = trackCursor + DDSF_TRACK_HEADER_BYTES;
        stats = {
          min: view.getFloat32(statsBase + DdsfTrackStatsOffsets.min, true),
          max: view.getFloat32(statsBase + DdsfTrackStatsOffsets.max, true),
          vpp: view.getFloat32(statsBase + DdsfTrackStatsOffsets.vpp, true),
          mean: view.getFloat32(statsBase + DdsfTrackStatsOffsets.mean, true),
          rms: view.getFloat32(statsBase + DdsfTrackStatsOffsets.rms, true),
          freqHz: view.getFloat32(statsBase + DdsfTrackStatsOffsets.freqHz, true),
          duty: view.getFloat32(statsBase + DdsfTrackStatsOffsets.duty, true),
        };
        payloadRelOffset += DDSF_TRACK_STATS_BYTES;
      }

      if (payloadRelOffset + dataByteLen > blockByteLen) {
        throw new DdsfParseError("track data_byte_len exceeds block_byte_len");
      }

      const absPayloadOffset = byteOffset + trackCursor + payloadRelOffset;
      let payload: DdsfTrackPayloadView;
      let blockKind: DdsfBlockKindCode;

      switch (rawKind) {
        case DdsfBlockKind.AnalogRawXY: {
          blockKind = DdsfBlockKind.AnalogRawXY;
          const expectedBytes = elementCount * DDSF_ANALOG_POINT_FLOATS * 4;
          if (dataByteLen < expectedBytes) {
            throw new DdsfParseError("AnalogRawXY payload smaller than elementCount * 8");
          }
          payload = {
            kind: DdsfBlockKind.AnalogRawXY,
            points: createFloat32View(
              buffer,
              absPayloadOffset,
              elementCount * DDSF_ANALOG_POINT_FLOATS,
            ),
          };
          break;
        }
        case DdsfBlockKind.AnalogM4Column: {
          blockKind = DdsfBlockKind.AnalogM4Column;
          const expectedBytes = elementCount * DDSF_M4_COLUMN_FLOATS * 4;
          if (dataByteLen < expectedBytes) {
            throw new DdsfParseError("AnalogM4Column payload smaller than elementCount * 20");
          }
          payload = {
            kind: DdsfBlockKind.AnalogM4Column,
            columns: createFloat32View(
              buffer,
              absPayloadOffset,
              elementCount * DDSF_M4_COLUMN_FLOATS,
            ),
          };
          break;
        }
        case DdsfBlockKind.DigitalEdges: {
          blockKind = DdsfBlockKind.DigitalEdges;
          const expectedBytes = elementCount * 4;
          if (dataByteLen < expectedBytes) {
            throw new DdsfParseError("DigitalEdges payload smaller than elementCount * 4");
          }
          payload = {
            kind: DdsfBlockKind.DigitalEdges,
            initLevel: digitalInitLevel,
            edges: createFloat32View(buffer, absPayloadOffset, elementCount),
          };
          break;
        }
        case DdsfBlockKind.StateRibbons: {
          blockKind = DdsfBlockKind.StateRibbons;
          const expectedBytes = elementCount * DDSF_STATE_RIBBON_BYTES;
          if (dataByteLen < expectedBytes) {
            throw new DdsfParseError("StateRibbons payload smaller than elementCount * 16");
          }
          const ribbonViews = createStateRibbonViews(buffer, absPayloadOffset, elementCount);
          payload = {
            kind: DdsfBlockKind.StateRibbons,
            floats: ribbonViews.floats,
            stateIds: ribbonViews.stateIds,
            colorIndices: ribbonViews.colorIndices,
          };
          break;
        }
        case DdsfBlockKind.SpectrumFrame: {
          blockKind = DdsfBlockKind.SpectrumFrame;
          const expectedBytes = elementCount * 4;
          if (dataByteLen < expectedBytes) {
            throw new DdsfParseError("SpectrumFrame payload smaller than elementCount * 4");
          }
          payload = {
            kind: DdsfBlockKind.SpectrumFrame,
            bins: createFloat32View(buffer, absPayloadOffset, elementCount),
            fStart: auxF32_0,
            df: auxF32_1,
          };
          break;
        }
        default:
          throw new DdsfParseError(`unknown DDSF block_kind: ${rawKind}`);
      }

      tracks[t] = {
        channelId,
        blockKind,
        digitalInitLevel,
        trackFlags,
        elementCount,
        dataByteLen,
        blockByteLen,
        auxF32_0,
        auxF32_1,
        stats,
        payload,
      };
      trackCursor += blockByteLen;
    }

    scopes[s] = {
      scopeId,
      triggerState,
      mode,
      trackBlockCount,
      winTStartNs,
      winTSpanNs,
      subpixelOffsetNs,
      packetByteLen,
      tracks,
    };
    cursor += packetByteLen;
  }

  const snapshots: DdsfSnapshotView[] = new Array(snapshotCount);
  for (let i = 0; i < snapshotCount; i += 1) {
    const snapOffset = snapshotsStart + i * DDSF_SNAPSHOT_ENTRY_BYTES;
    const channelNumId = view.getUint32(snapOffset + DdsfSnapshotOffsets.channelNumId, true);
    const kind = view.getUint8(snapOffset + DdsfSnapshotOffsets.kind) as DdsfSnapshotKindCode;
    const valF64 = view.getFloat64(snapOffset + DdsfSnapshotOffsets.valF64, true);
    snapshots[i] = {
      channelNumId,
      kind,
      valF64,
    };
  }

  return {
    header,
    scopes,
    snapshots,
    byteLength: totalByteLen,
  };
}

// ============================================================================
// DDSF v1 Binary Frame Builder (matches Rust DdsfFramePacker)
// ============================================================================

export type DdsfTrackBuildInput =
  | {
      channelId: number;
      blockKind: typeof DdsfBlockKind.AnalogRawXY;
      trackFlags?: number;
      stats?: DdsfTrackStatsView | null;
      /** Packed `[x_norm, y_val]` pairs (`2 * elementCount` floats). */
      points: Float32Array | number[];
      auxF32_0?: number;
      auxF32_1?: number;
    }
  | {
      channelId: number;
      blockKind: typeof DdsfBlockKind.AnalogM4Column;
      trackFlags?: number;
      stats?: DdsfTrackStatsView | null;
      /** Packed `[x_norm, v_first, v_min, v_max, v_last]` tuples (`5 * elementCount` floats). */
      columns: Float32Array | number[];
      auxF32_0?: number;
      auxF32_1?: number;
    }
  | {
      channelId: number;
      blockKind: typeof DdsfBlockKind.DigitalEdges;
      digitalInitLevel: 0 | 1;
      trackFlags?: number;
      stats?: DdsfTrackStatsView | null;
      edges: Float32Array | number[];
      auxF32_0?: number;
      auxF32_1?: number;
    }
  | {
      channelId: number;
      blockKind: typeof DdsfBlockKind.StateRibbons;
      trackFlags?: number;
      stats?: DdsfTrackStatsView | null;
      ribbons: ReadonlyArray<{
        x0Norm: number;
        x1Norm: number;
        stateId: number;
        colorIdx: number;
      }>;
      auxF32_0?: number;
      auxF32_1?: number;
    }
  | {
      channelId: number;
      blockKind: typeof DdsfBlockKind.SpectrumFrame;
      trackFlags?: number;
      stats?: DdsfTrackStatsView | null;
      bins: Float32Array | number[];
      fStart: number;
      df: number;
    };

export interface DdsfScopeBuildInput {
  scopeId: number;
  triggerState: number;
  mode: number;
  winTStartNs: number;
  winTSpanNs: number;
  subpixelOffsetNs: number;
  tracks: DdsfTrackBuildInput[];
}

export interface DdsfSnapshotBuildInput {
  channelNumId: number;
  kind: DdsfSnapshotKindCode;
  valF64: number;
}

export interface DdsfFrameBuildInput {
  frameSeq: number;
  hostNowNs: number;
  flags?: number;
  scopes: DdsfScopeBuildInput[];
  snapshots: DdsfSnapshotBuildInput[];
}

function getTrackElementCountAndDataLen(track: DdsfTrackBuildInput): {
  elementCount: number;
  dataByteLen: number;
} {
  switch (track.blockKind) {
    case DdsfBlockKind.AnalogRawXY: {
      const elementCount = Math.floor(track.points.length / DDSF_ANALOG_POINT_FLOATS);
      return {
        elementCount,
        dataByteLen: elementCount * DDSF_ANALOG_POINT_FLOATS * 4,
      };
    }
    case DdsfBlockKind.AnalogM4Column: {
      const elementCount = Math.floor(track.columns.length / DDSF_M4_COLUMN_FLOATS);
      return {
        elementCount,
        dataByteLen: elementCount * DDSF_M4_COLUMN_FLOATS * 4,
      };
    }
    case DdsfBlockKind.DigitalEdges: {
      const elementCount = track.edges.length;
      return {
        elementCount,
        dataByteLen: elementCount * 4,
      };
    }
    case DdsfBlockKind.StateRibbons: {
      const elementCount = track.ribbons.length;
      return {
        elementCount,
        dataByteLen: elementCount * DDSF_STATE_RIBBON_BYTES,
      };
    }
    case DdsfBlockKind.SpectrumFrame: {
      const elementCount = track.bins.length;
      return {
        elementCount,
        dataByteLen: elementCount * 4,
      };
    }
  }
}

/**
 * Packs a `DDSF v1` frame into a tightly-aligned little-endian `ArrayBuffer`.
 */
export class DdsfFrameBuilder {
  private scratch: ArrayBuffer;

  constructor(initialBytes = 64 * 1024) {
    this.scratch = new ArrayBuffer(Math.max(4096, initialBytes));
  }

  /**
   * Computes exact byte length for a frame input before serialization.
   */
  static computeByteLength(input: DdsfFrameBuildInput): number {
    let total = DDSF_HEADER_BYTES;
    for (let s = 0; s < input.scopes.length; s += 1) {
      const scope = input.scopes[s];
      total += DDSF_SCOPE_HEADER_BYTES;
      for (let t = 0; t < scope.tracks.length; t += 1) {
        const track = scope.tracks[t];
        const hasStats = Boolean(track.stats) || ((track.trackFlags ?? 0) & DDSF_TRACK_FLAG_HAS_STATS) !== 0;
        const statsBytes = hasStats ? DDSF_TRACK_STATS_BYTES : 0;
        const { dataByteLen } = getTrackElementCountAndDataLen(track);
        total += DDSF_TRACK_HEADER_BYTES + statsBytes + alignUp8(dataByteLen);
      }
    }
    total += input.snapshots.length * DDSF_SNAPSHOT_ENTRY_BYTES;
    return total;
  }

  /**
   * Serializes into a standalone `ArrayBuffer` (suitable for Worker transfer or standalone tests).
   * Optionally reuses `targetBuffer` if its capacity is sufficient.
   */
  pack(input: DdsfFrameBuildInput, targetBuffer?: ArrayBuffer): ArrayBuffer {
    const totalByteLen = DdsfFrameBuilder.computeByteLength(input);
    const outBuffer =
      targetBuffer && targetBuffer.byteLength >= totalByteLen
        ? targetBuffer
        : new ArrayBuffer(totalByteLen);

    this.packInto(input, outBuffer, totalByteLen);
    return outBuffer.byteLength === totalByteLen ? outBuffer : outBuffer.slice(0, totalByteLen);
  }

  /**
   * Serializes into the internal scratch buffer and returns a slice copy of exact length.
   */
  packToSlice(input: DdsfFrameBuildInput): ArrayBuffer {
    const totalByteLen = DdsfFrameBuilder.computeByteLength(input);
    if (this.scratch.byteLength < totalByteLen) {
      this.scratch = new ArrayBuffer(Math.max(totalByteLen, this.scratch.byteLength * 2));
    }
    this.packInto(input, this.scratch, totalByteLen);
    return this.scratch.slice(0, totalByteLen);
  }

  private packInto(input: DdsfFrameBuildInput, buffer: ArrayBuffer, totalByteLen: number): void {
    const u8 = new Uint8Array(buffer, 0, totalByteLen);
    u8.fill(0);
    const view = new DataView(buffer, 0, totalByteLen);

    // 1. Root 64B header
    view.setUint32(DdsfHeaderOffsets.magic, DDSF_MAGIC, true);
    view.setUint16(DdsfHeaderOffsets.version, DDSF_VERSION_V1, true);
    view.setUint16(DdsfHeaderOffsets.flags, input.flags ?? 0, true);
    view.setUint32(DdsfHeaderOffsets.frameSeq, input.frameSeq >>> 0, true);
    view.setUint16(DdsfHeaderOffsets.scopeCount, input.scopes.length, true);
    view.setUint16(DdsfHeaderOffsets.snapshotCount, input.snapshots.length, true);
    writeU64FromNumber(view, DdsfHeaderOffsets.hostNowNs, input.hostNowNs);
    writeU64FromNumber(view, DdsfHeaderOffsets.totalByteLen, totalByteLen);

    let cursor = DDSF_HEADER_BYTES;

    // 2. Scope packets
    for (let s = 0; s < input.scopes.length; s += 1) {
      const scope = input.scopes[s];
      const scopeStart = cursor;

      view.setUint32(scopeStart + DdsfScopeHeaderOffsets.scopeId, scope.scopeId >>> 0, true);
      view.setUint8(scopeStart + DdsfScopeHeaderOffsets.triggerState, scope.triggerState & 0xff);
      view.setUint8(scopeStart + DdsfScopeHeaderOffsets.mode, scope.mode & 0xff);
      view.setUint16(scopeStart + DdsfScopeHeaderOffsets.trackBlockCount, scope.tracks.length, true);
      writeU64FromNumber(view, scopeStart + DdsfScopeHeaderOffsets.winTStartNs, scope.winTStartNs);
      writeU64FromNumber(view, scopeStart + DdsfScopeHeaderOffsets.winTSpanNs, scope.winTSpanNs);
      view.setFloat32(
        scopeStart + DdsfScopeHeaderOffsets.subpixelOffsetNs,
        scope.subpixelOffsetNs,
        true,
      );

      cursor += DDSF_SCOPE_HEADER_BYTES;

      for (let t = 0; t < scope.tracks.length; t += 1) {
        const track = scope.tracks[t];
        const blockStart = cursor;
        const hasStats =
          Boolean(track.stats) || ((track.trackFlags ?? 0) & DDSF_TRACK_FLAG_HAS_STATS) !== 0;
        let trackFlags = track.trackFlags ?? 0;
        if (hasStats) {
          trackFlags |= DDSF_TRACK_FLAG_HAS_STATS;
        }

        const { elementCount, dataByteLen } = getTrackElementCountAndDataLen(track);
        const alignedDataLen = alignUp8(dataByteLen);
        const statsBytes = hasStats ? DDSF_TRACK_STATS_BYTES : 0;
        const blockByteLen = DDSF_TRACK_HEADER_BYTES + statsBytes + alignedDataLen;

        const aux0 =
          track.blockKind === DdsfBlockKind.SpectrumFrame
            ? track.fStart
            : (track.auxF32_0 ?? 0);
        const aux1 =
          track.blockKind === DdsfBlockKind.SpectrumFrame
            ? track.df
            : (track.auxF32_1 ?? 0);
        const digitalInitLevel =
          track.blockKind === DdsfBlockKind.DigitalEdges ? track.digitalInitLevel : 0;

        view.setUint32(blockStart + DdsfTrackHeaderOffsets.channelId, track.channelId >>> 0, true);
        view.setUint8(blockStart + DdsfTrackHeaderOffsets.blockKind, track.blockKind);
        view.setUint8(blockStart + DdsfTrackHeaderOffsets.digitalInitLevel, digitalInitLevel);
        view.setUint16(blockStart + DdsfTrackHeaderOffsets.trackFlags, trackFlags, true);
        view.setUint32(blockStart + DdsfTrackHeaderOffsets.elementCount, elementCount >>> 0, true);
        view.setUint32(blockStart + DdsfTrackHeaderOffsets.dataByteLen, dataByteLen >>> 0, true);
        view.setUint32(blockStart + DdsfTrackHeaderOffsets.blockByteLen, blockByteLen >>> 0, true);
        view.setFloat32(blockStart + DdsfTrackHeaderOffsets.auxF32_0, aux0, true);
        view.setFloat32(blockStart + DdsfTrackHeaderOffsets.auxF32_1, aux1, true);
        view.setUint32(blockStart + DdsfTrackHeaderOffsets.reserved, 0, true);

        let writeCursor = blockStart + DDSF_TRACK_HEADER_BYTES;
        if (hasStats) {
          const stats = track.stats ?? {
            min: 0,
            max: 0,
            vpp: 0,
            mean: 0,
            rms: 0,
            freqHz: 0,
            duty: 0,
          };
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.min, stats.min, true);
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.max, stats.max, true);
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.vpp, stats.vpp, true);
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.mean, stats.mean, true);
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.rms, stats.rms, true);
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.freqHz, stats.freqHz, true);
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.duty, stats.duty, true);
          view.setFloat32(writeCursor + DdsfTrackStatsOffsets.reserved, 0, true);
          writeCursor += DDSF_TRACK_STATS_BYTES;
        }

        switch (track.blockKind) {
          case DdsfBlockKind.AnalogRawXY: {
            const floatCount = elementCount * DDSF_ANALOG_POINT_FLOATS;
            for (let i = 0; i < floatCount; i += 1) {
              view.setFloat32(writeCursor + i * 4, track.points[i], true);
            }
            break;
          }
          case DdsfBlockKind.AnalogM4Column: {
            const floatCount = elementCount * DDSF_M4_COLUMN_FLOATS;
            for (let i = 0; i < floatCount; i += 1) {
              view.setFloat32(writeCursor + i * 4, track.columns[i], true);
            }
            break;
          }
          case DdsfBlockKind.DigitalEdges: {
            for (let i = 0; i < elementCount; i += 1) {
              view.setFloat32(writeCursor + i * 4, track.edges[i], true);
            }
            break;
          }
          case DdsfBlockKind.StateRibbons: {
            for (let i = 0; i < elementCount; i += 1) {
              const r = track.ribbons[i];
              const base = writeCursor + i * DDSF_STATE_RIBBON_BYTES;
              view.setFloat32(base, r.x0Norm, true);
              view.setFloat32(base + 4, r.x1Norm, true);
              view.setInt32(base + 8, r.stateId, true);
              view.setUint32(base + 12, r.colorIdx >>> 0, true);
            }
            break;
          }
          case DdsfBlockKind.SpectrumFrame: {
            for (let i = 0; i < elementCount; i += 1) {
              view.setFloat32(writeCursor + i * 4, track.bins[i], true);
            }
            break;
          }
        }

        cursor += blockByteLen;
      }

      const packetByteLen = cursor - scopeStart;
      view.setUint32(scopeStart + DdsfScopeHeaderOffsets.packetByteLen, packetByteLen >>> 0, true);
    }

    // 3. Snapshots
    for (let i = 0; i < input.snapshots.length; i += 1) {
      const snap = input.snapshots[i];
      view.setUint32(cursor + DdsfSnapshotOffsets.channelNumId, snap.channelNumId >>> 0, true);
      view.setUint8(cursor + DdsfSnapshotOffsets.kind, snap.kind & 0xff);
      view.setFloat64(cursor + DdsfSnapshotOffsets.valF64, snap.valF64, true);
      cursor += DDSF_SNAPSHOT_ENTRY_BYTES;
    }
  }
}
