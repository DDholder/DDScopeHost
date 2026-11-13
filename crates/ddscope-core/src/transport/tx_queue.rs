//! Dual-priority downlink transmission queue with O(1) in-place payload coalescing.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::{OutboundFrame, OutboundReceiver, ShutdownReceiver, TxPriority};
use crate::types::{CoreError, CoreResult};

/// Default maximum number of pending outbound frames retained in [`TxCoalescingQueue`].
pub const DEFAULT_TX_QUEUE_CAPACITY: usize = 1_024;

/// Receipt returned when submitting a frame into [`TxCoalescingQueue`] or [`SharedTxQueue`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxEnqueueReceipt {
    /// `true` when an unsent frame with the same `coalesce_key` was overwritten in-place.
    pub coalesced_existing: bool,
    /// Total pending frames in the queue after this operation.
    pub queue_len: usize,
    /// Effective priority lane holding the queued frame.
    pub effective_priority: TxPriority,
}

/// Cumulative counters for [`TxCoalescingQueue`] operations.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxQueueStats {
    pub total_pushed: u64,
    pub coalesced_overwrites: u64,
    pub total_popped: u64,
    pub rejected_full: u64,
}

/// Dual-priority (`High` + `Normal`) TX queue supporting $O(1)$ in-place replacement of stale
/// unsent frames sharing the same `coalesce_key`.
#[derive(Debug)]
pub struct TxCoalescingQueue {
    max_capacity: usize,
    slots: Vec<Option<OutboundFrame>>,
    free_slots: Vec<usize>,
    high_fifo: VecDeque<usize>,
    normal_fifo: VecDeque<usize>,
    high_len: usize,
    normal_len: usize,
    key_index: HashMap<String, usize>,
    stats: TxQueueStats,
}

impl Default for TxCoalescingQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl TxCoalescingQueue {
    /// Creates a queue with [`DEFAULT_TX_QUEUE_CAPACITY`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_TX_QUEUE_CAPACITY)
    }

    /// Creates a queue bounded to `max_capacity` distinct pending frames.
    pub fn with_capacity(max_capacity: usize) -> Self {
        let initial_reserve = max_capacity.min(128);
        Self {
            max_capacity,
            slots: Vec::with_capacity(initial_reserve),
            free_slots: Vec::new(),
            high_fifo: VecDeque::with_capacity(initial_reserve),
            normal_fifo: VecDeque::with_capacity(initial_reserve),
            high_len: 0,
            normal_len: 0,
            key_index: HashMap::new(),
            stats: TxQueueStats::default(),
        }
    }

    /// Returns the configured maximum distinct frame capacity.
    #[inline]
    pub fn max_capacity(&self) -> usize {
        self.max_capacity
    }

    /// Returns the total number of queued frames across both priority lanes.
    #[inline]
    pub fn len(&self) -> usize {
        self.high_len + self.normal_len
    }

    /// Returns the number of queued [`TxPriority::High`] frames.
    #[inline]
    pub fn high_len(&self) -> usize {
        self.high_len
    }

    /// Returns the number of queued [`TxPriority::Normal`] frames.
    #[inline]
    pub fn normal_len(&self) -> usize {
        self.normal_len
    }

    /// Returns the number of distinct `coalesce_key` entries currently pending in the queue.
    #[inline]
    pub fn coalesced_key_count(&self) -> usize {
        self.key_index.len()
    }

    /// Returns `true` when zero frames are pending.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns `true` when an unsent frame with `key` is currently queued.
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.key_index.contains_key(key)
    }

    /// Returns a reference to the queued frame for `key`, if present.
    pub fn get_by_key(&self, key: &str) -> Option<&OutboundFrame> {
        let &slot_idx = self.key_index.get(key)?;
        self.slots[slot_idx].as_ref()
    }

    /// Peeks at the next frame that [`Self::pop_next`] would return without removing it.
    pub fn peek_next(&self) -> Option<&OutboundFrame> {
        for &idx in &self.high_fifo {
            if let Some(ref frame) = self.slots[idx] {
                return Some(frame);
            }
        }
        for &idx in &self.normal_fifo {
            if let Some(ref frame) = self.slots[idx] {
                return Some(frame);
            }
        }
        None
    }

    /// Returns cumulative push/coalesce/pop/reject statistics.
    #[inline]
    pub fn stats(&self) -> TxQueueStats {
        self.stats
    }

    /// Clears all pending frames while retaining backing capacities.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free_slots.clear();
        self.high_fifo.clear();
        self.normal_fifo.clear();
        self.high_len = 0;
        self.normal_len = 0;
        self.key_index.clear();
    }

    #[inline]
    fn alloc_slot(&mut self) -> usize {
        if let Some(idx) = self.free_slots.pop() {
            idx
        } else {
            let idx = self.slots.len();
            self.slots.push(None);
            idx
        }
    }

    /// Enqueues `frame`, overwriting any unsent frame sharing its `coalesce_key` in-place.
    pub fn push(&mut self, frame: OutboundFrame) -> CoreResult<TxEnqueueReceipt> {
        if let Some(key) = frame.coalesce_key.as_deref() {
            if let Some(&slot_idx) = self.key_index.get(key) {
                let existing_prio = self.slots[slot_idx]
                    .as_ref()
                    .expect("key_index points to live slot")
                    .priority;

                if existing_prio == frame.priority
                    || (existing_prio == TxPriority::High && frame.priority == TxPriority::Normal)
                {
                    let existing = self.slots[slot_idx]
                        .as_mut()
                        .expect("key_index points to live slot");
                    existing.payload = frame.payload;
                    existing.transaction_id = frame.transaction_id;
                    self.stats.total_pushed += 1;
                    self.stats.coalesced_overwrites += 1;
                    return Ok(TxEnqueueReceipt {
                        coalesced_existing: true,
                        queue_len: self.len(),
                        effective_priority: existing_prio,
                    });
                }

                // Promote from Normal lane to High lane while removing the stale Normal entry.
                self.slots[slot_idx] = None;
                self.free_slots.push(slot_idx);
                self.normal_len -= 1;

                if self.normal_fifo.len() > 4 * self.normal_len.max(16) {
                    let slots = &self.slots;
                    self.normal_fifo.retain(|&idx| slots[idx].is_some());
                }

                let new_idx = self.alloc_slot();
                if let Some(entry) = self.key_index.get_mut(key) {
                    *entry = new_idx;
                }
                self.slots[new_idx] = Some(frame);
                self.high_fifo.push_back(new_idx);
                self.high_len += 1;

                self.stats.total_pushed += 1;
                self.stats.coalesced_overwrites += 1;
                return Ok(TxEnqueueReceipt {
                    coalesced_existing: true,
                    queue_len: self.len(),
                    effective_priority: TxPriority::High,
                });
            }
        }

        if self.len() >= self.max_capacity {
            self.stats.rejected_full += 1;
            return Err(CoreError::Transport(format!(
                "TX coalescing queue full (capacity={})",
                self.max_capacity
            )));
        }

        let slot_idx = self.alloc_slot();
        let effective_priority = frame.priority;
        if let Some(ref key) = frame.coalesce_key {
            self.key_index.insert(key.clone(), slot_idx);
        }
        match effective_priority {
            TxPriority::High => {
                self.high_fifo.push_back(slot_idx);
                self.high_len += 1;
            }
            TxPriority::Normal => {
                self.normal_fifo.push_back(slot_idx);
                self.normal_len += 1;
            }
        }
        self.slots[slot_idx] = Some(frame);
        self.stats.total_pushed += 1;

        Ok(TxEnqueueReceipt {
            coalesced_existing: false,
            queue_len: self.len(),
            effective_priority,
        })
    }

    fn pop_from_fifo(
        fifo: &mut VecDeque<usize>,
        slots: &mut [Option<OutboundFrame>],
        free_slots: &mut Vec<usize>,
        key_index: &mut HashMap<String, usize>,
        lane_len: &mut usize,
    ) -> Option<OutboundFrame> {
        while let Some(idx) = fifo.pop_front() {
            if let Some(frame) = slots[idx].take() {
                *lane_len -= 1;
                free_slots.push(idx);
                if let Some(ref key) = frame.coalesce_key {
                    key_index.remove(key);
                }
                return Some(frame);
            }
        }
        None
    }

    /// Removes and returns the next frame (`High` priority strictly precedes `Normal`, FIFO within each lane).
    pub fn pop_next(&mut self) -> Option<OutboundFrame> {
        let popped = Self::pop_from_fifo(
            &mut self.high_fifo,
            &mut self.slots,
            &mut self.free_slots,
            &mut self.key_index,
            &mut self.high_len,
        )
        .or_else(|| {
            Self::pop_from_fifo(
                &mut self.normal_fifo,
                &mut self.slots,
                &mut self.free_slots,
                &mut self.key_index,
                &mut self.normal_len,
            )
        });

        if popped.is_some() {
            self.stats.total_popped += 1;
        }
        popped
    }

    /// Drains all immediately ready frames from `rx` into this queue.
    pub fn drain_receiver(&mut self, rx: &mut OutboundReceiver) -> CoreResult<usize> {
        let mut count = 0usize;
        while let Ok(frame) = rx.try_recv() {
            self.push(frame)?;
            count += 1;
        }
        Ok(count)
    }
}

/// Thread-safe, async-notifying wrapper around [`TxCoalescingQueue`].
#[derive(Clone, Debug)]
pub struct SharedTxQueue {
    inner: Arc<Mutex<TxCoalescingQueue>>,
    notify: Arc<Notify>,
    closed: Arc<AtomicBool>,
}

impl Default for SharedTxQueue {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

impl SharedTxQueue {
    /// Creates a shared queue bounded to `max_capacity` distinct pending frames.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TxCoalescingQueue::with_capacity(max_capacity))),
            notify: Arc::new(Notify::new()),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Creates a shared queue with [`DEFAULT_TX_QUEUE_CAPACITY`].
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_TX_QUEUE_CAPACITY)
    }

    /// Submits a frame into the shared queue and wakes the TX pump task.
    pub fn submit(&self, frame: OutboundFrame) -> CoreResult<TxEnqueueReceipt> {
        if self.closed.load(Ordering::Acquire) {
            return Err(CoreError::ChannelClosed);
        }
        let receipt = self.inner.lock().push(frame)?;
        self.notify.notify_one();
        Ok(receipt)
    }

    /// Pops the next highest-priority frame if available.
    pub fn pop_next(&self) -> Option<OutboundFrame> {
        self.inner.lock().pop_next()
    }

    /// Returns total pending frames across both priority lanes.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Returns pending [`TxPriority::High`] frames.
    pub fn high_len(&self) -> usize {
        self.inner.lock().high_len()
    }

    /// Returns pending [`TxPriority::Normal`] frames.
    pub fn normal_len(&self) -> usize {
        self.inner.lock().normal_len()
    }

    /// Returns the number of distinct active `coalesce_key` entries.
    pub fn coalesced_key_count(&self) -> usize {
        self.inner.lock().coalesced_key_count()
    }

    /// Returns `true` when zero frames are pending.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Returns `true` when an unsent frame with `key` is pending.
    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.lock().contains_key(key)
    }

    /// Returns a snapshot of queue counters.
    pub fn stats(&self) -> TxQueueStats {
        self.inner.lock().stats()
    }

    /// Clears all pending frames from the queue.
    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    /// Closes the queue against new submissions and wakes all waiting consumers.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Re-opens the queue for submissions after a transport restart.
    pub fn reopen(&self) {
        self.closed.store(false, Ordering::Release);
    }

    /// Returns `true` if the queue is currently closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Waits without lost wakeups until at least one frame is queued, or returns `false` on
    /// shutdown or queue closure.
    pub(crate) async fn wait_until_non_empty(&self, shutdown_rx: &mut ShutdownReceiver) -> bool {
        loop {
            if *shutdown_rx.borrow() {
                return false;
            }
            if !self.is_empty() {
                return true;
            }
            if self.is_closed() {
                return false;
            }

            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if *shutdown_rx.borrow() {
                return false;
            }
            if !self.is_empty() {
                return true;
            }
            if self.is_closed() {
                return false;
            }

            tokio::select! {
                _ = &mut notified => {}
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        return false;
                    }
                }
            }
        }
    }
}

/// Transport-side consumer helper that drains any burst of frames ready on an [`OutboundReceiver`]
/// and yields them in priority + coalesced order.
#[derive(Debug)]
pub struct TxStreamConsumer {
    rx: OutboundReceiver,
    staging: TxCoalescingQueue,
}

impl TxStreamConsumer {
    /// Wraps `rx` with an unbounded staging [`TxCoalescingQueue`].
    pub fn new(rx: OutboundReceiver) -> Self {
        Self {
            rx,
            staging: TxCoalescingQueue::with_capacity(usize::MAX),
        }
    }

    /// Waits for the next outbound frame, draining and coalescing any concurrently ready burst
    /// before popping the highest-priority frame.
    pub async fn recv_next(&mut self) -> Option<OutboundFrame> {
        if self.staging.is_empty() {
            let first = self.rx.recv().await?;
            let _ = self.staging.push(first);
        }
        while let Ok(next) = self.rx.try_recv() {
            let _ = self.staging.push(next);
        }
        self.staging.pop_next()
    }

    /// Returns the number of frames already drained into the local staging queue.
    #[inline]
    pub fn pending_len(&self) -> usize {
        self.staging.len()
    }

    /// Returns cumulative statistics of the local staging queue.
    #[inline]
    pub fn stats(&self) -> TxQueueStats {
        self.staging.stats()
    }

    /// Unwraps and returns the underlying [`OutboundReceiver`].
    pub fn into_inner(self) -> OutboundReceiver {
        self.rx
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[test]
    fn high_frequency_same_key_keeps_depth_one_and_latest_value() {
        let mut queue = TxCoalescingQueue::new();
        for i in 0_u32..10_000 {
            let payload = Bytes::from(format!("kp={i}"));
            let receipt = queue
                .push(OutboundFrame::coalesced(payload, "uart1:kp"))
                .expect("coalescing push should succeed");
            assert_eq!(receipt.queue_len, 1);
            assert_eq!(receipt.coalesced_existing, i > 0);
            assert_eq!(queue.len(), 1);
            assert_eq!(queue.coalesced_key_count(), 1);
        }

        let popped = queue.pop_next().expect("final frame must exist");
        assert_eq!(&popped.payload[..], b"kp=9999");
        assert!(queue.pop_next().is_none());
        assert_eq!(queue.stats().coalesced_overwrites, 9_999);
    }

    #[test]
    fn interleaved_keys_and_unkeyed_preserve_fifo_slots() {
        let mut queue = TxCoalescingQueue::new();
        queue
            .push(OutboundFrame::fire_and_forget(
                Bytes::from_static(b"CMD_A"),
                TxPriority::Normal,
            ))
            .unwrap();
        queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"kp=1"),
                "kp",
            ))
            .unwrap();
        queue
            .push(OutboundFrame::fire_and_forget(
                Bytes::from_static(b"CMD_B"),
                TxPriority::Normal,
            ))
            .unwrap();
        queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"ki=10"),
                "ki",
            ))
            .unwrap();
        queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"kp=2"),
                "kp",
            ))
            .unwrap();

        assert_eq!(queue.len(), 4);
        assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"CMD_A"));
        assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"kp=2"));
        assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"CMD_B"));
        assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"ki=10"));
        assert!(queue.is_empty());
    }

    #[test]
    fn priority_promotion_and_high_lane_preservation() {
        let mut queue = TxCoalescingQueue::new();
        queue
            .push(OutboundFrame::fire_and_forget(
                Bytes::from_static(b"NORM_1"),
                TxPriority::Normal,
            ))
            .unwrap();
        let r1 = queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"kp=1"),
                "kp",
            ))
            .unwrap();
        assert_eq!(r1.effective_priority, TxPriority::Normal);

        let r2 = queue
            .push(OutboundFrame::coalesced_with_priority(
                Bytes::from_static(b"kp=0"),
                "kp",
                TxPriority::High,
            ))
            .unwrap();
        assert!(r2.coalesced_existing);
        assert_eq!(r2.effective_priority, TxPriority::High);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.high_len(), 1);
        assert_eq!(queue.normal_len(), 1);

        // Subsequent normal update for same key keeps High priority lane.
        let r3 = queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"kp=2"),
                "kp",
            ))
            .unwrap();
        assert!(r3.coalesced_existing);
        assert_eq!(r3.effective_priority, TxPriority::High);

        assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"kp=2"));
        assert_eq!(queue.pop_next().unwrap().payload, Bytes::from_static(b"NORM_1"));
        assert!(queue.pop_next().is_none());
    }

    #[test]
    fn full_queue_rejects_new_entries_but_allows_existing_key_overwrite() {
        let mut queue = TxCoalescingQueue::with_capacity(2);
        queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"kp=1"),
                "kp",
            ))
            .unwrap();
        queue
            .push(OutboundFrame::fire_and_forget(
                Bytes::from_static(b"PING"),
                TxPriority::Normal,
            ))
            .unwrap();

        let err = queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"ki=1"),
                "ki",
            ))
            .expect_err("new key must fail when queue is full");
        assert!(matches!(err, CoreError::Transport(_)));

        let overwrite = queue
            .push(OutboundFrame::coalesced(
                Bytes::from_static(b"kp=99"),
                "kp",
            ))
            .expect("existing key overwrite must succeed even at full capacity");
        assert!(overwrite.coalesced_existing);
        assert_eq!(overwrite.queue_len, 2);
        assert_eq!(queue.get_by_key("kp").unwrap().payload, Bytes::from_static(b"kp=99"));
    }

    #[tokio::test]
    async fn tx_stream_consumer_coalesces_concurrent_burst() {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        for i in 0_u32..5 {
            tx.send(OutboundFrame::coalesced(
                Bytes::from(format!("kp={i}")),
                "kp",
            ))
            .await
            .unwrap();
        }
        tx.send(OutboundFrame::fire_and_forget(
            Bytes::from_static(b"ESTOP"),
            TxPriority::High,
        ))
        .await
        .unwrap();
        drop(tx);

        let mut consumer = TxStreamConsumer::new(rx);
        let first = consumer.recv_next().await.expect("ESTOP arrives first");
        assert_eq!(first.payload, Bytes::from_static(b"ESTOP"));
        let second = consumer.recv_next().await.expect("coalesced kp arrives second");
        assert_eq!(second.payload, Bytes::from_static(b"kp=4"));
        assert!(consumer.recv_next().await.is_none());
    }
}
