//! Core scalar types, channel descriptors, columnar chunks, and SoA batch contracts.

pub mod batch;
pub mod chunk;
pub mod error;
pub mod ids;
pub mod scalar;
pub mod schema;

pub use batch::{DecodedBatch, DecodedColumnBuf, InboundAckEvent};
pub use chunk::{
    AlignedBuffer, AlignedVec, BitVecChunk, CacheLineBlock, ChunkHandle, ChunkM4Levels,
    ChunkStorageRef, DataChunk, EventDataChunk, FrameDataChunk, M4Bucket, MmapChunkLocator,
    NativeScalarColumn, ScalarDataChunk, TimestampColumn, CACHE_LINE_BYTES, CHUNK_LEN,
    M4_BRANCH_FACTOR,
};
pub use error::{CoreError, CoreResult};
pub use ids::{ChannelId, ChannelPath, ScopeId, SourceId, SyncGroupId, TimestampNs};
pub use scalar::{Endianness, LinearScale, ScalarSlice, ScalarType, ScalarValue};
pub use schema::{
    AxisMeta, BitfieldSpec, ChannelDescriptor, ChannelKind, EventSchemaKind, StateEnumMap,
    StateLabel, StructFieldDef, StructFieldKind, StructSchema,
};

#[cfg(test)]
mod tests {
    use std::mem::size_of;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn aligned_containers_satisfy_64_byte_alignment() {
        let mut buf = AlignedBuffer::with_capacity(128);
        buf.extend_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.as_ptr() as usize % CACHE_LINE_BYTES, 0);

        buf.resize(100, 0xAA);
        assert_eq!(buf.len(), 100);
        assert_eq!(buf[99], 0xAA);

        let mut vec_f32: AlignedVec<f32> = AlignedVec::with_capacity(32);
        vec_f32.push(1.5);
        vec_f32.extend_from_slice(&[2.5, 3.5]);
        assert_eq!(vec_f32.len(), 3);
        assert_eq!(vec_f32.as_ptr() as usize % CACHE_LINE_BYTES, 0);
        assert_eq!(vec_f32.as_slice(), &[1.5, 2.5, 3.5]);
    }

    #[test]
    fn scalar_type_bit_widths_and_byte_sizes_are_accurate() {
        assert_eq!(ScalarType::Bool.bit_width(), 1);
        assert_eq!(ScalarType::Bool.byte_size_for_len(0), 0);
        assert_eq!(ScalarType::Bool.byte_size_for_len(1), 8);
        assert_eq!(ScalarType::Bool.byte_size_for_len(64), 8);
        assert_eq!(ScalarType::Bool.byte_size_for_len(65), 16);

        assert_eq!(ScalarType::U8.byte_size_for_len(10), 10);
        assert_eq!(ScalarType::I16.byte_size_for_len(10), 20);
        assert_eq!(ScalarType::F32.byte_size_for_len(10), 40);
        assert_eq!(ScalarType::F64.byte_size_for_len(10), 80);

        let scale = LinearScale::new(0.5, -10.0);
        assert!(!scale.is_identity());
        assert_eq!(scale.apply(40.0), 10.0);
        assert_eq!(scale.apply_f32(40.0), 10.0_f32);
    }

    #[test]
    fn timestamp_column_and_chunk_storage_ref_behave_consistently() {
        let mut rel = AlignedVec::with_capacity(4);
        rel.extend_from_slice(&[0_u32, 100, 200]);
        let ts = TimestampColumn::RelU32 {
            base_ts_ns: 1_000_000,
            rel_ns: rel,
        };
        assert_eq!(ts.first_ns(), Some(1_000_000));
        assert_eq!(ts.last_ns(), Some(1_000_200));
        assert_eq!(ts.get_ns(1), Some(1_000_100));

        let mut vals = AlignedVec::with_capacity(4);
        vals.extend_from_slice(&[10_i16, 20, 30]);
        let chunk = Arc::new(DataChunk::Scalar(
            ScalarDataChunk::new(ts, NativeScalarColumn::I16(vals), LinearScale::identity())
                .expect("valid scalar chunk"),
        ));
        assert_eq!(chunk.sample_count(), 3);
        assert_eq!(chunk.time_range_ns(), Some((1_000_000, 1_000_200)));

        let mem_ref = ChunkStorageRef::InMemory(Arc::clone(&chunk));
        assert!(mem_ref.is_in_memory());
        assert!(mem_ref.resident_bytes() > 0);

        let mmap_ref = ChunkStorageRef::MmapSlice(MmapChunkLocator {
            file_id: 1,
            byte_offset: 4096,
            byte_len: 1024,
            flags: 0,
        });
        assert!(!mmap_ref.is_in_memory());
        assert_eq!(mmap_ref.resident_bytes(), 0);
    }

    #[test]
    fn m4_bucket_is_32_bytes_and_pod() {
        assert_eq!(size_of::<M4Bucket>(), 32);
        let bucket = M4Bucket {
            t_first_ns: 10,
            t_last_ns: 20,
            v_first: 1.0,
            v_min: -2.0,
            v_max: 3.0,
            v_last: 1.5,
        };
        let bytes = bytemuck::bytes_of(&bucket);
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn decoded_batch_clear_retains_column_capacity() {
        let desc = ChannelDescriptor::new_scalar("sim0", "ia", ScalarType::F32);
        let mut batch = DecodedBatch::from_descriptors(&[desc], 128);
        assert_eq!(batch.columns.len(), 1);

        if let DecodedColumnBuf::F32(col) = &mut batch.columns[0] {
            col.extend_from_slice(&[1.0, 2.0, 3.0]);
            batch.sample_rows = 3;
            assert!(col.capacity() >= 128);
        } else {
            panic!("expected F32 column");
        }

        batch.clear();
        assert!(batch.is_empty());
        if let DecodedColumnBuf::F32(col) = &batch.columns[0] {
            assert_eq!(col.len(), 0);
            assert!(col.capacity() >= 128);
        }
    }
}
