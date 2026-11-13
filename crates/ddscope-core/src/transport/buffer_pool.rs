//! Slab-allocated, zero-fragmentation `BytesMut` packet buffer pool for transport RX paths.

use std::collections::VecDeque;
use std::sync::Arc;

use bytes::buf::Limit;
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use super::RawPacket;
use crate::types::{TimestampNs, CACHE_LINE_BYTES};

/// Standard OS virtual memory page size in bytes (`4 KiB`).
pub const OS_PAGE_BYTES: usize = 4 * 1024;

/// Default slab capacity per allocation (`64 KiB`, 16 OS pages).
pub const DEFAULT_SLAB_BYTES: usize = 64 * 1024;

/// Default number of slabs pre-allocated per source (`1 MiB`).
pub const DEFAULT_PREALLOC_SLABS: usize = 16;

/// Default upper bound on retained slabs in the per-source recycle ring (`4 MiB`).
pub const DEFAULT_MAX_RETAINED_SLABS: usize = 64;

/// Configuration parameters for [`PacketBufferPool`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferPoolConfig {
    /// Usable byte capacity per slab (rounded up to a multiple of [`OS_PAGE_BYTES`]).
    pub slab_bytes: usize,
    /// Number of slabs pre-allocated when the pool is constructed.
    pub prealloc_slabs: usize,
    /// Maximum number of recycled slabs kept in the pool's ring buffer.
    pub max_retained_slabs: usize,
}

impl Default for BufferPoolConfig {
    fn default() -> Self {
        Self {
            slab_bytes: DEFAULT_SLAB_BYTES,
            prealloc_slabs: DEFAULT_PREALLOC_SLABS,
            max_retained_slabs: DEFAULT_MAX_RETAINED_SLABS,
        }
    }
}

impl BufferPoolConfig {
    /// Returns a normalized copy with page-aligned slab size and consistent retention bounds.
    pub fn normalized(self) -> Self {
        let slab_bytes = self
            .slab_bytes
            .max(OS_PAGE_BYTES)
            .div_ceil(OS_PAGE_BYTES)
            * OS_PAGE_BYTES;
        let max_retained_slabs = self.max_retained_slabs.max(self.prealloc_slabs).max(1);
        Self {
            slab_bytes,
            prealloc_slabs: self.prealloc_slabs,
            max_retained_slabs,
        }
    }
}

/// Telemetry counters describing [`PacketBufferPool`] utilization and reclamation.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferPoolStats {
    pub slab_bytes: usize,
    pub pooled_slabs: usize,
    pub active_spare_bytes: usize,
    pub total_packets_frozen: u64,
    pub total_bytes_frozen: u64,
    pub slabs_allocated: u64,
    pub fallback_allocations: u64,
    pub slabs_reclaimed: u64,
}

/// Page-aligned `BytesMut` slab pool that slices immutable `Bytes` views via `split().freeze()`
/// and reclaims exhausted slabs in-place once downstream `RawPacket` holders drop them.
#[derive(Debug)]
pub struct PacketBufferPool {
    config: BufferPoolConfig,
    active: BytesMut,
    recycle_ring: VecDeque<BytesMut>,
    total_packets_frozen: u64,
    total_bytes_frozen: u64,
    slabs_allocated: u64,
    fallback_allocations: u64,
    slabs_reclaimed: u64,
}

impl Default for PacketBufferPool {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketBufferPool {
    /// Creates a pool with default `64 KiB` slabs and `16` pre-allocated slabs.
    pub fn new() -> Self {
        Self::with_config(BufferPoolConfig::default())
    }

    /// Creates a pool with the specified slab byte size and pre-allocated slab count.
    pub fn with_slab_size(slab_bytes: usize, prealloc_slabs: usize) -> Self {
        Self::with_config(BufferPoolConfig {
            slab_bytes,
            prealloc_slabs,
            max_retained_slabs: prealloc_slabs.max(DEFAULT_MAX_RETAINED_SLABS),
        })
    }

    /// Creates a pool using the provided [`BufferPoolConfig`].
    pub fn with_config(config: BufferPoolConfig) -> Self {
        let config = config.normalized();
        let prealloc = config.prealloc_slabs.max(1);
        let active = Self::allocate_arc_slab(config.slab_bytes);
        let mut recycle_ring = VecDeque::with_capacity(config.max_retained_slabs);
        for _ in 1..prealloc {
            recycle_ring.push_back(Self::allocate_arc_slab(config.slab_bytes));
        }
        Self {
            config,
            active,
            recycle_ring,
            total_packets_frozen: 0,
            total_bytes_frozen: 0,
            slabs_allocated: prealloc as u64,
            fallback_allocations: 0,
            slabs_reclaimed: 0,
        }
    }

    /// Allocates a slab promoted to shared `KIND_ARC` storage with a 64-byte tail guard so
    /// `split().freeze()` never detaches the active slab handle from its backing allocation.
    fn allocate_arc_slab(usable_bytes: usize) -> BytesMut {
        let raw_cap = usable_bytes + CACHE_LINE_BYTES;
        let mut slab = BytesMut::with_capacity(raw_cap);
        slab.put_u8(0);
        let _anchor = slab.split();
        let _ = slab.try_reclaim(raw_cap);
        slab
    }

    /// Returns the normalized configuration in effect for this pool.
    pub fn config(&self) -> BufferPoolConfig {
        self.config
    }

    /// Returns the configured usable byte size of a standard slab.
    pub fn slab_bytes(&self) -> usize {
        self.config.slab_bytes
    }

    /// Returns a point-in-time snapshot of pool allocation and reclamation statistics.
    pub fn stats(&self) -> BufferPoolStats {
        BufferPoolStats {
            slab_bytes: self.config.slab_bytes,
            pooled_slabs: self.recycle_ring.len(),
            active_spare_bytes: self
                .active
                .capacity()
                .saturating_sub(self.active.len() + CACHE_LINE_BYTES),
            total_packets_frozen: self.total_packets_frozen,
            total_bytes_frozen: self.total_bytes_frozen,
            slabs_allocated: self.slabs_allocated,
            fallback_allocations: self.fallback_allocations,
            slabs_reclaimed: self.slabs_reclaimed,
        }
    }

    /// Discards any uncommitted bytes currently sitting in the active slab.
    pub fn clear_active(&mut self) {
        if !self.active.is_empty() {
            self.active.clear();
        }
    }

    fn ensure_writable(&mut self, min_writable_bytes: usize) {
        self.clear_active();
        let needed = min_writable_bytes.max(1);
        if self.active.capacity() >= needed + CACHE_LINE_BYTES {
            return;
        }

        let target_usable = needed.max(self.config.slab_bytes).div_ceil(OS_PAGE_BYTES) * OS_PAGE_BYTES;
        let required_raw = target_usable + CACHE_LINE_BYTES;

        let mut next_slab = None;
        for _ in 0..self.recycle_ring.len() {
            if let Some(mut candidate) = self.recycle_ring.pop_front() {
                candidate.clear();
                if candidate.capacity() >= required_raw || candidate.try_reclaim(required_raw) {
                    self.slabs_reclaimed += 1;
                    next_slab = Some(candidate);
                    break;
                }
                self.recycle_ring.push_back(candidate);
            }
        }

        let replacement = match next_slab {
            Some(slab) => slab,
            None => {
                self.slabs_allocated += 1;
                self.fallback_allocations += 1;
                Self::allocate_arc_slab(target_usable)
            }
        };

        let old = std::mem::replace(&mut self.active, replacement);
        if self.recycle_ring.len() < self.config.max_retained_slabs {
            self.recycle_ring.push_back(old);
        }
    }

    /// Ensures the active slab can accept at least `min_writable_bytes` without consuming the
    /// 64-byte `KIND_ARC` guard tail, and returns a mutable reference to it.
    pub fn acquire_buf(&mut self, min_writable_bytes: usize) -> &mut BytesMut {
        self.ensure_writable(min_writable_bytes);
        &mut self.active
    }

    /// Returns a capacity-limited mutable view suitable for `AsyncReadExt::read_buf` or
    /// `UdpSocket::recv_buf`, preventing a single read from consuming the guard tail.
    pub fn acquire_limited_buf(&mut self, max_writable_bytes: usize) -> Limit<&mut BytesMut> {
        let cap = max_writable_bytes.max(1);
        self.acquire_buf(cap).limit(cap)
    }

    /// Freezes all bytes currently written to the active slab into an immutable [`RawPacket`].
    ///
    /// Returns `None` if no bytes have been written since the previous freeze or clear.
    pub fn freeze_packet(
        &mut self,
        host_recv_ns: TimestampNs,
        source_tag: Option<Arc<str>>,
    ) -> Option<RawPacket> {
        if self.active.is_empty() {
            return None;
        }
        let bytes = self.active.split().freeze();
        self.total_packets_frozen += 1;
        self.total_bytes_frozen += bytes.len() as u64;
        Some(RawPacket {
            bytes,
            host_recv_ns,
            source_tag,
        })
    }

    /// Copies `data` into the active slab and freezes it into a zero-copy [`RawPacket`].
    pub fn pack_slice(
        &mut self,
        data: &[u8],
        host_recv_ns: TimestampNs,
        source_tag: Option<Arc<str>>,
    ) -> RawPacket {
        if data.is_empty() {
            self.total_packets_frozen += 1;
            return RawPacket {
                bytes: Bytes::new(),
                host_recv_ns,
                source_tag,
            };
        }
        self.acquire_buf(data.len()).extend_from_slice(data);
        self.freeze_packet(host_recv_ns, source_tag)
            .expect("non-empty slice always freezes a packet")
    }

    /// Invokes `read_fn` directly on a pre-sized writable slice from the pool and freezes the
    /// populated prefix into a [`RawPacket`], returning `Ok(None)` on EOF (`0` bytes read).
    pub fn read_into_packet<F, E>(
        &mut self,
        max_bytes: usize,
        host_recv_ns: TimestampNs,
        source_tag: Option<Arc<str>>,
        read_fn: F,
    ) -> Result<Option<RawPacket>, E>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, E>,
    {
        if max_bytes == 0 {
            return Ok(None);
        }
        let buf = self.acquire_buf(max_bytes);
        buf.resize(max_bytes, 0);
        match read_fn(&mut buf[..max_bytes]) {
            Ok(0) => {
                self.active.clear();
                Ok(None)
            }
            Ok(n) => {
                let actual = n.min(max_bytes);
                self.active.truncate(actual);
                Ok(self.freeze_packet(host_recv_ns, source_tag))
            }
            Err(err) => {
                self.active.clear();
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_normalizes_to_page_boundaries() {
        let cfg = BufferPoolConfig {
            slab_bytes: 5000,
            prealloc_slabs: 4,
            max_retained_slabs: 2,
        }
        .normalized();
        assert_eq!(cfg.slab_bytes, 2 * OS_PAGE_BYTES);
        assert_eq!(cfg.max_retained_slabs, 4);
    }

    #[test]
    fn slab_pool_reclaims_without_fallback_allocations_when_packets_drop() {
        let mut pool = PacketBufferPool::with_slab_size(DEFAULT_SLAB_BYTES, 4);
        assert_eq!(pool.slab_bytes(), DEFAULT_SLAB_BYTES);
        assert_eq!(pool.stats().pooled_slabs, 3);

        // Stream 1024-byte packets across ~16 full slab rotations while dropping each packet.
        for seq in 0_u32..1_024 {
            let payload = vec![(seq & 0xFF) as u8; 1_024];
            let pkt = pool.pack_slice(&payload, seq as u64 * 1_000, None);
            assert_eq!(pkt.bytes.len(), 1_024);
            assert_eq!(pkt.bytes[0], (seq & 0xFF) as u8);
            drop(pkt);
        }

        let stats = pool.stats();
        assert_eq!(stats.total_packets_frozen, 1_024);
        assert_eq!(stats.total_bytes_frozen, 1_024 * 1_024);
        assert_eq!(stats.fallback_allocations, 0);
        assert!(stats.slabs_reclaimed >= 12);
    }

    #[test]
    fn oversize_packet_promotes_slab_and_preserves_contents() {
        let mut pool = PacketBufferPool::with_slab_size(OS_PAGE_BYTES, 2);
        let big = vec![0xA5_u8; 12_000];
        let pkt = pool.pack_slice(&big, 42, Some(Arc::from("udp0")));
        assert_eq!(pkt.bytes.len(), 12_000);
        assert!(pkt.bytes.iter().all(|&b| b == 0xA5));
        assert_eq!(pkt.host_recv_ns, 42);
        assert_eq!(pkt.source_tag.as_deref(), Some("udp0"));
        assert_eq!(pool.stats().fallback_allocations, 1);
    }

    #[test]
    fn read_into_packet_handles_data_eof_and_error() {
        let mut pool = PacketBufferPool::with_slab_size(OS_PAGE_BYTES, 2);

        let pkt = pool
            .read_into_packet::<_, std::io::Error>(64, 100, None, |buf| {
                buf[..5].copy_from_slice(b"HELLO");
                Ok(5)
            })
            .expect("read should succeed")
            .expect("non-zero read yields packet");
        assert_eq!(&pkt.bytes[..], b"HELLO");

        let eof = pool
            .read_into_packet::<_, std::io::Error>(64, 200, None, |_buf| Ok(0))
            .expect("eof should succeed");
        assert!(eof.is_none());

        let err = pool.read_into_packet::<_, std::io::Error>(64, 300, None, |buf| {
            buf[0] = 0xFF;
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed"))
        });
        assert!(err.is_err());
        assert!(pool.freeze_packet(301, None).is_none());
    }

    #[test]
    fn limited_buf_respects_requested_cap_and_freezes_cleanly() {
        let mut pool = PacketBufferPool::with_slab_size(OS_PAGE_BYTES, 2);
        {
            let mut limited = pool.acquire_limited_buf(16);
            assert_eq!(limited.remaining_mut(), 16);
            limited.put_slice(b"ABCD1234");
        }
        let pkt = pool.freeze_packet(999, None).expect("packet should freeze");
        assert_eq!(&pkt.bytes[..], b"ABCD1234");
        assert_eq!(pkt.host_recv_ns, 999);
    }
}
