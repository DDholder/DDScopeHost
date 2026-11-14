import test from "node:test";
import assert from "node:assert/strict";
import {
  DdsfFrameBuilder,
  DdsfParseError,
  parseDdsfFrame,
} from "../src/bridge/ddsf-parser.ts";
import {
  DDSF_HEADER_BYTES,
  DDSF_MAGIC,
  DDSF_TRACK_FLAG_HAS_STATS,
  DDSF_VERSION_V1,
  DdsfBlockKind,
  DdsfSnapshotKind,
  TriggerRuntimeStateCode,
} from "../src/bridge/ddsf-spec.ts";

test("DDSF v1 empty frame round-trip", () => {
  const builder = new DdsfFrameBuilder();
  const buf = builder.pack({
    frameSeq: 7,
    hostNowNs: 123_456_789_012,
    flags: 0x0003,
    scopes: [],
    snapshots: [],
  });

  assert.equal(buf.byteLength, DDSF_HEADER_BYTES);
  const parsed = parseDdsfFrame(buf);
  assert.equal(parsed.header.magic, DDSF_MAGIC);
  assert.equal(parsed.header.version, DDSF_VERSION_V1);
  assert.equal(parsed.header.flags, 0x0003);
  assert.equal(parsed.header.frameSeq, 7);
  assert.equal(parsed.header.scopeCount, 0);
  assert.equal(parsed.header.snapshotCount, 0);
  assert.equal(parsed.header.hostNowNs, 123_456_789_012);
  assert.equal(parsed.header.totalByteLen, DDSF_HEADER_BYTES);
  assert.equal(parsed.scopes.length, 0);
  assert.equal(parsed.snapshots.length, 0);
});

test("DDSF v1 multi-scope all 5 block kinds + stats + 8B padding + snapshots round-trip", () => {
  const builder = new DdsfFrameBuilder();
  const buf = builder.pack({
    frameSeq: 42,
    hostNowNs: 9_876_543_210_123,
    scopes: [
      {
        scopeId: 1,
        triggerState: TriggerRuntimeStateCode.Roll,
        mode: 0,
        winTStartNs: 5_000_000_000,
        winTSpanNs: 20_000_000,
        subpixelOffsetNs: -125.5,
        tracks: [
          {
            channelId: 101,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: new Float32Array([0.0, -5.0, 0.5, 12.25, 1.0, -5.0]), // 3 pts = 24B (already 8B aligned)
            stats: {
              min: -5.0,
              max: 12.25,
              vpp: 17.25,
              mean: 0.75,
              rms: 7.42,
              freqHz: 50.0,
              duty: 0.5,
            },
          },
          {
            channelId: 102,
            blockKind: DdsfBlockKind.AnalogM4Column,
            columns: new Float32Array([
              0.0, 48.0, 47.5, 48.5, 48.1,
              0.5, 48.1, 47.2, 128.0, 48.3,
              1.0, 48.3, 47.9, 48.8, 48.0,
            ]), // 3 cols = 60B -> padded to 64B
          },
          {
            channelId: 105,
            blockKind: DdsfBlockKind.DigitalEdges,
            digitalInitLevel: 1,
            edges: [0.15, 0.42, 0.78], // odd count 3 floats = 12B -> padded to 16B
          },
          {
            channelId: 108,
            blockKind: DdsfBlockKind.StateRibbons,
            ribbons: [
              { x0Norm: 0.0, x1Norm: 0.35, stateId: 0, colorIdx: 0 },
              { x0Norm: 0.35, x1Norm: 0.7, stateId: 2, colorIdx: 2 },
              { x0Norm: 0.7, x1Norm: 1.0, stateId: 3, colorIdx: 3 },
            ],
          },
        ],
      },
      {
        scopeId: 2,
        triggerState: TriggerRuntimeStateCode.LatchedPinned,
        mode: 1,
        winTStartNs: 1_000_000,
        winTSpanNs: 2_000,
        subpixelOffsetNs: 0.25,
        tracks: [
          {
            channelId: 109,
            blockKind: DdsfBlockKind.SpectrumFrame,
            bins: [-80.0, -45.5, -12.0, -50.0, -78.25], // 5 floats = 20B -> padded to 24B
            fStart: 100.0,
            df: 250.0,
          },
        ],
      },
    ],
    snapshots: [
      { channelNumId: 101, kind: DdsfSnapshotKind.Scalar, valF64: 12.25 },
      { channelNumId: 105, kind: DdsfSnapshotKind.Bool, valF64: 1.0 },
      { channelNumId: 108, kind: DdsfSnapshotKind.StateEnum, valF64: 3.0 },
    ],
  });

  const frame = parseDdsfFrame(buf);
  assert.equal(frame.header.byteLength ?? buf.byteLength, frame.header.totalByteLen);
  assert.equal(frame.scopes.length, 2);
  assert.equal(frame.snapshots.length, 3);

  const s1 = frame.scopes[0];
  assert.equal(s1.scopeId, 1);
  assert.equal(s1.triggerState, TriggerRuntimeStateCode.Roll);
  assert.equal(s1.subpixelOffsetNs, -125.5);
  assert.equal(s1.tracks.length, 4);

  const tRaw = s1.tracks[0];
  assert.equal(tRaw.blockKind, DdsfBlockKind.AnalogRawXY);
  assert.ok((tRaw.trackFlags & DDSF_TRACK_FLAG_HAS_STATS) !== 0);
  assert.ok(tRaw.stats !== null);
  assert.equal(tRaw.stats.max, 12.25);
  assert.equal(tRaw.elementCount, 3);
  assert.equal(tRaw.payload.kind, DdsfBlockKind.AnalogRawXY);
  if (tRaw.payload.kind === DdsfBlockKind.AnalogRawXY) {
    assert.equal(tRaw.payload.points[3], 12.25);
  }

  const tM4 = s1.tracks[1];
  assert.equal(tM4.stats, null);
  assert.equal(tM4.payload.kind, DdsfBlockKind.AnalogM4Column);
  if (tM4.payload.kind === DdsfBlockKind.AnalogM4Column) {
    assert.equal(tM4.payload.columns[5 + 3], 128.0);
  }

  const tDig = s1.tracks[2];
  assert.equal(tDig.payload.kind, DdsfBlockKind.DigitalEdges);
  if (tDig.payload.kind === DdsfBlockKind.DigitalEdges) {
    assert.equal(tDig.payload.initLevel, 1);
    assert.equal(tDig.payload.edges.length, 3);
    assert.ok(Math.abs(tDig.payload.edges[1] - 0.42) < 1e-6);
  }

  const tState = s1.tracks[3];
  assert.equal(tState.payload.kind, DdsfBlockKind.StateRibbons);
  if (tState.payload.kind === DdsfBlockKind.StateRibbons) {
    assert.equal(tState.payload.stateIds[2 * 4 + 2], 3);
    assert.equal(tState.payload.colorIndices[2 * 4 + 3], 3);
  }

  const s2 = frame.scopes[1];
  assert.equal(s2.triggerState, TriggerRuntimeStateCode.LatchedPinned);
  const tSpec = s2.tracks[0];
  assert.equal(tSpec.payload.kind, DdsfBlockKind.SpectrumFrame);
  if (tSpec.payload.kind === DdsfBlockKind.SpectrumFrame) {
    assert.equal(tSpec.payload.fStart, 100.0);
    assert.equal(tSpec.payload.df, 250.0);
    assert.equal(tSpec.payload.bins[2], -12.0);
  }

  assert.equal(frame.snapshots[2].channelNumId, 108);
  assert.equal(frame.snapshots[2].kind, DdsfSnapshotKind.StateEnum);
  assert.equal(frame.snapshots[2].valF64, 3);
});

test("DDSF v1 unaligned byteOffset slice copy fallback and error guards", () => {
  const builder = new DdsfFrameBuilder();
  const packed = builder.pack({
    frameSeq: 9,
    hostNowNs: 1_000_000,
    scopes: [
      {
        scopeId: 3,
        triggerState: TriggerRuntimeStateCode.Armed,
        mode: 0,
        winTStartNs: 0,
        winTSpanNs: 10_000,
        subpixelOffsetNs: 0,
        tracks: [
          {
            channelId: 101,
            blockKind: DdsfBlockKind.AnalogRawXY,
            points: [0.25, 3.5, 0.75, -2.5],
          },
        ],
      },
    ],
    snapshots: [],
  });

  // Offset by +2 bytes (unaligned for Float32Array)
  const padded = new Uint8Array(packed.byteLength + 6);
  padded.set(new Uint8Array(packed), 2);
  const parsed = parseDdsfFrame(padded.buffer, 2, packed.byteLength);
  assert.equal(parsed.scopes.length, 1);
  const payload = parsed.scopes[0].tracks[0].payload;
  assert.equal(payload.kind, DdsfBlockKind.AnalogRawXY);
  if (payload.kind === DdsfBlockKind.AnalogRawXY) {
    assert.equal(payload.points[1], 3.5);
  }

  assert.throws(() => parseDdsfFrame(new ArrayBuffer(16)), DdsfParseError);

  const badMagic = new Uint8Array(packed.slice(0));
  badMagic[0] = 0x00;
  assert.throws(() => parseDdsfFrame(badMagic.buffer), DdsfParseError);

  const badVer = new DataView(packed.slice(0));
  badVer.setUint16(4, 99, true);
  assert.throws(() => parseDdsfFrame(badVer.buffer), DdsfParseError);

  assert.throws(
    () => parseDdsfFrame(packed, 0, packed.byteLength - 4),
    DdsfParseError,
  );
});
