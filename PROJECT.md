# Project: DDScopeHost — Phase 1 Track A (TA1)

## Architecture
DDScopeHost Phase 1 Track A (TA1) establishes high-performance, native-width columnar storage for multi-modal oscilloscope and logic analyzer telemetry.
- **`crates/ddscope-core/src/types/chunk.rs`**: Core data structures:
  - Cache-line aligned vector `AlignedVec<T: Pod>` (64 bytes).
  - Native-width scalar column `NativeScalarColumn` supporting 10 primitive types (`i8, i16, i32, i64, u8, u16, u32, u64, f32, f64`) without premature widening to `f64`.
  - 1-bit packed `BitVecChunk` packing 65,536 boolean logic samples into 1,024 `u64` words (8,192 bytes) with pre-computed intra-chunk transition edge indices in `Vec<u16>`.
  - Timestamp column `TimestampColumn` dual encoding: `RelU32` for time spans $\le 4.29$s (50% RAM savings) and `AbsU64` for longer spans.
  - Event series data chunk `EventDataChunk::StateTransitions` holding parallel timestamp and state code arrays.
  - Immutable sealed chunk payloads `ScalarDataChunk`, `EventDataChunk`, `DataChunk`.
- **`crates/ddscope-core/src/storage/`**: Series management:
  - `NativeScalarSeries`: Manages active pre-allocated append buffer (`CHUNK_LEN = 65,536`), automatic sealing into immutable `Arc<DataChunk>`, and sealed chunk history (`Vec<ChunkHandle>`).
  - `EventSeries`: Manages state transition event buffers, monotonic validation, and sealed chunks.
  - Query subsystem: Global sample retrieval by index `(0..total_samples)` and by timestamp `TimestampNs`.
  - Watermark engine: Dispatches `WatermarkEvent` (`channel_id, latest_ts_ns, total_samples`) to registered `WatermarkListener`s upon batch commit.
- **`crates/ddscope-core/benches/storage_append.rs`**: Criterion benchmark harness:
  - Benchmarks batch ingestion throughput exceeding 10M samples/sec on native scalar data.

## Feature Inventory
| # | Feature | Description | Milestone | Source |
|---|---------|-------------|-----------|--------|
| 1 | `CHUNK_LEN` Constant | Fixed sample capacity per sealed chunk ($65,536$) | M1 | `04-phase1-design-and-plan.md` §3.4 |
| 2 | `CACHE_LINE_BYTES` Alignment | 64-byte alignment for cache line locality | M1 | `04-phase1-design-and-plan.md` §3.4 |
| 3 | 10 Native Scalar Types | Native-width storage (`i8, i16, i32, i64, u8, u16, u32, u64, f32, f64`) | M1 | `ORIGINAL_REQUEST.md` R1 |
| 4 | `AlignedVec<T: Pod>` | 64-byte aligned typed vector container | M1 | `crates/ddscope-core/src/types/chunk.rs` |
| 5 | `TimestampColumn` Dual Encoding | `RelU32` for $\le 4.29$s, `AbsU64` for $> 4.29$s | M1 | `04-phase1-design-and-plan.md` §3.4 |
| 6 | `ScalarDataChunk` | Immutable payload: timestamps, values, linear scale | M1 | `crates/ddscope-core/src/types/chunk.rs` |
| 7 | `LinearScale` Calibration | Physical transform `raw * scale + offset` | M1 | `crates/ddscope-core/src/types/scalar.rs` |
| 8 | 1-Bit Packed `BitVecChunk` | Sample-accurate 1-bit boolean storage in `u64` words | M1 | `ORIGINAL_REQUEST.md` R2 |
| 9 | Pre-computed Edge Indices | Transition indices ($\text{bit}[k] \ne \text{bit}[k-1]$) in `Vec<u16>` | M1 | `ORIGINAL_REQUEST.md` R2 |
| 10 | `BitVecChunk` Sample Access | $O(1)$ random bit lookup via word mask | M1 | `04-phase1-design-and-plan.md` §3.4 |
| 11 | Logic Initial Level Tracking | Initial left-edge state `digital_init_level` (0 or 1) | M1 | `crates/ddscope-core/src/scope/ddsf.rs` |
| 12 | `EventDataChunk::StateTransitions` | Parallel timestamp and state code arrays | M1 | `ORIGINAL_REQUEST.md` R3 |
| 13 | State Transition Appending | Timestamped state code appending with monotonic check | M1 | `ORIGINAL_REQUEST.md` R3 |
| 14 | State Transition Retrieval | $O(1)$ by index, $O(\log N)$ by timestamp binary search | M1 | `ORIGINAL_REQUEST.md` R3 |
| 15 | Serde RC Fix | Enable `"rc"` feature in root `Cargo.toml` for `Arc<StructSchema>` | M1 | Survey diagnosis |
| 16 | `ActiveScalarChunk` | Pre-allocated 65,536 buffer with batch slice append | M2 | `04-phase1-design-and-plan.md` §2.3 |
| 17 | Automatic Sealing at 65,536 | Sealing full chunk into immutable `Arc<DataChunk>` | M2 | `ORIGINAL_REQUEST.md` R1 |
| 18 | Multi-Batch Rollover | Batch splitting across 65,536 chunk boundaries | M2 | `ORIGINAL_REQUEST.md` R1 |
| 19 | `NativeScalarSeries` | Storage series managing active buffer and sealed history | M2 | `ORIGINAL_REQUEST.md` R4 |
| 20 | `EventSeries` | Storage series for event state transitions | M2 | `ORIGINAL_REQUEST.md` R4 |
| 21 | Sample Retrieval by Index | Global index lookup across sealed chunks and active buffer | M2 | `ORIGINAL_REQUEST.md` R4 |
| 22 | Sample Retrieval by Timestamp | Binary search by timestamp across sealed and active chunks | M2 | `ORIGINAL_REQUEST.md` R4 |
| 23 | Watermark Notification Emission | Commit notification (`WatermarkEvent`) to listeners | M2 | `ORIGINAL_REQUEST.md` R4 |
| 24 | `ChunkStorageRef` Isolation | `InMemory(Arc<DataChunk>)` vs `MmapSlice` locator | M2 | `04-phase1-design-and-plan.md` §3.4 |
| 25 | Criterion Append Benchmark | Throughput benchmark $> 10\text{ M samples/sec}$ on native data | M3 | `ORIGINAL_REQUEST.md` Acceptance |
| 26 | Workspace Quality Gate | Zero clippy warnings & 100% test pass on `cargo test -p ddscope-core` | M3 | `ORIGINAL_REQUEST.md` Acceptance |

## Milestones
| # | Name | Scope | Dependencies | Status |
|---|------|-------|-------------|--------|
| M1 | Chunk Data Types & Primitives | Implement native-width scalar columns (10 types), BitVecChunk with word-level bit-packing and edge index extraction, EventDataChunk state transitions, and fix serde build blocker. | none | DONE |
| M2 | Storage Series & Watermark Engine | Implement `NativeScalarSeries`, `EventSeries`, active chunk buffer, 65,536 automatic sealing to `Arc<DataChunk>`, global sample retrieval by index/timestamp, and watermark notification emission. | M1 | DONE |
| M3 | Performance Benchmark & Hardening | Implement Criterion benchmark in `benches/storage_append.rs`, verify > 10M samples/sec append throughput, pass all unit tests and clippy with 0 warnings. | M2 | DONE |
| E2E | E2E Testing Track | Independent requirement-driven test suite (Tiers 1-4) verifying all 26 features across equivalence classes, boundaries, interactions, and realistic workloads; publish `TEST_READY.md`. | none | DONE |

## Interface Contracts

### `types::chunk` ↔ `storage`
- `NativeScalarColumn::with_capacity(scalar_type: ScalarType, cap: usize) -> Self`
- `NativeScalarColumn::append_slice(&mut self, slice: ScalarSlice) -> Result<usize, CoreError>`
- `NativeScalarColumn::get_as_f64(&self, idx: usize) -> Option<f64>`
- `BitVecChunk::with_capacity(bit_cap: usize) -> Self`
- `BitVecChunk::push(&mut self, val: bool)`
- `BitVecChunk::extend_from_slice(&mut self, bits: &[bool])`
- `BitVecChunk::compute_edge_indices(&mut self)`
- `BitVecChunk::get(&self, idx: usize) -> Option<bool>`
- `TimestampColumn::from_timestamps(base: TimestampNs, ts: &[TimestampNs]) -> Self`
- `TimestampColumn::get_ns(&self, idx: usize) -> Option<TimestampNs>`
- `EventDataChunk::StateTransitions { timestamps_ns: AlignedVec<u64>, state_codes: AlignedVec<i32> }`
- `EventDataChunk::append_transition(&mut self, ts_ns: TimestampNs, code: i32) -> Result<(), CoreError>`
- `EventDataChunk::get_state_at(&self, ts_ns: TimestampNs) -> Option<i32>`
- `ScalarDataChunk::new(timestamps: TimestampColumn, values: NativeScalarColumn, calibration: LinearScale) -> Result<Self, CoreError>`

### `storage` ↔ Consumers / E2E
- `NativeScalarSeries::new(channel_id: ChannelId, scalar_type: ScalarType, scale: LinearScale) -> Self`
- `NativeScalarSeries::append(&mut self, timestamps: &[TimestampNs], values: ScalarSlice) -> Result<usize, CoreError>`
- `NativeScalarSeries::total_samples(&self) -> u64`
- `NativeScalarSeries::sealed_chunk_count(&self) -> usize`
- `NativeScalarSeries::get_sample_by_index(&self, index: u64) -> Option<(TimestampNs, f64)>`
- `NativeScalarSeries::get_sample_by_timestamp(&self, ts_ns: TimestampNs) -> Option<(u64, TimestampNs, f64)>`
- `NativeScalarSeries::register_listener(&mut self, listener: Arc<dyn WatermarkListener>)`
- `EventSeries::new(channel_id: ChannelId) -> Self`
- `EventSeries::append(&mut self, timestamps: &[TimestampNs], state_codes: &[i32]) -> Result<usize, CoreError>`
- `EventSeries::get_state_at(&self, ts_ns: TimestampNs) -> Option<i32>`
- `EventSeries::register_listener(&mut self, listener: Arc<dyn WatermarkListener>)`

## Code Layout
- `Cargo.toml`: Root workspace configuration, serde `"rc"` feature addition.
- `crates/ddscope-core/Cargo.toml`: `ddscope-core` dependencies and `criterion` dev-dependency.
- `crates/ddscope-core/src/types/chunk.rs`: Chunk definitions, aligned buffers, `NativeScalarColumn`, `BitVecChunk`, `TimestampColumn`, `EventDataChunk`, `ScalarDataChunk`. Owned by M1 Worker.
- `crates/ddscope-core/src/storage/mod.rs`: Storage module entry point, re-exports, watermark types. Owned by M2 Worker.
- `crates/ddscope-core/src/storage/scalar_series.rs`: `NativeScalarSeries` active buffer, sealing, sample queries, watermark dispatch. Owned by M2 Worker.
- `crates/ddscope-core/src/storage/event_series.rs`: `EventSeries` transition series, active buffer, queries, watermark dispatch. Owned by M2 Worker.
- `crates/ddscope-core/benches/storage_append.rs`: Criterion append benchmark harness. Owned by M3 Worker.
- `crates/ddscope-core/tests/e2e_storage_tests.rs`: Opaque-box E2E test suite (Tiers 1-4). Owned by E2E Testing Worker.
