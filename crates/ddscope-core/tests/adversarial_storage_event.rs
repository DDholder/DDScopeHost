//! Adversarial Empirical Stress & Invariant Test Suite for `EventSeries` and the Watermark Engine.
//!
//! Subsystem under test: `ddscope-core::storage::event_series`, `ddscope-core::storage::WatermarkListener`
//! Attacks:
//! 1. Hostile event transition sequences (simultaneous timestamps, rapid oscillations across 65,536 chunks,
//!    zero-order hold boundaries, extreme state codes, extreme timestamps, transaction rollbacks).
//! 2. Watermark Engine Stress (thousands of rapid commits, multi-series multi-listener fanouts,
//!    concurrent multi-threaded listener invocations, empty batch silence, multi-chunk rollover events).
//! 3. Sealing & Interleaving (micro-chunks, idempotent empty seals, mixed auto/manual seals,
//!    deterministic randomized fuzz oracle).
//! 4. Series Lifecycle & Invariant Tracking (cloning, range tracking, index lookups).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ddscope_core::storage::{EventSeries, WatermarkEvent, WatermarkListener};
use ddscope_core::types::{ChannelId, CoreError, TimestampNs, CHUNK_LEN};

// ============================================================================
// Test Utilities & Synthetic Oracles
// ============================================================================

/// Deterministic 64-bit XorShift PRNG for reproducible randomized fuzz tests without external crates.
#[derive(Debug, Clone)]
struct FuzzRng {
    state: u64,
}

impl FuzzRng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x853c49e6748fea9b } else { seed },
        }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    #[inline]
    fn next_range(&mut self, min: usize, max: usize) -> usize {
        assert!(min <= max);
        if min == max {
            return min;
        }
        let span = (max - min) + 1;
        min + (self.next_u64() as usize % span)
    }

    #[inline]
    fn next_i32(&mut self) -> i32 {
        self.next_u64() as i32
    }

    #[inline]
    fn next_bool(&mut self, prob_percent: u64) -> bool {
        (self.next_u64() % 100) < prob_percent
    }
}

/// Thread-safe watermark listener recording all dispatched events per channel.
#[derive(Default)]
struct StressWatermarkListener {
    events_by_channel: Mutex<HashMap<ChannelId, Vec<WatermarkEvent>>>,
    total_calls: AtomicUsize,
    total_event_records: AtomicUsize,
}

impl StressWatermarkListener {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn channel_events(&self, channel_id: ChannelId) -> Vec<WatermarkEvent> {
        let guard = self.events_by_channel.lock().unwrap();
        guard.get(&channel_id).cloned().unwrap_or_default()
    }

    fn channel_event_count(&self, channel_id: ChannelId) -> usize {
        let guard = self.events_by_channel.lock().unwrap();
        guard.get(&channel_id).map(|v| v.len()).unwrap_or(0)
    }

    fn total_calls(&self) -> usize {
        self.total_calls.load(Ordering::SeqCst)
    }

    fn total_event_records(&self) -> usize {
        self.total_event_records.load(Ordering::SeqCst)
    }
}

impl WatermarkListener for StressWatermarkListener {
    fn on_watermark_advanced(&self, events: &[WatermarkEvent]) {
        self.total_calls.fetch_add(1, Ordering::SeqCst);
        self.total_event_records.fetch_add(events.len(), Ordering::SeqCst);

        let mut guard = self.events_by_channel.lock().unwrap();
        for ev in events {
            guard.entry(ev.channel_id).or_default().push(*ev);
        }
    }
}

// ============================================================================
// Module 1: Hostile Event Transition Sequences
// ============================================================================
mod hostile_event_transitions {
    use super::*;

    #[test]
    fn test_simultaneous_events_varying_states_in_single_batch() {
        let mut series = EventSeries::new(ChannelId(1));

        // 10 simultaneous events at ts = 50_000, state stepping 1..=10
        let ts = vec![50_000u64; 10];
        let states: Vec<i32> = (1..=10).collect();

        assert_eq!(series.append(&ts, &states).unwrap(), 10);
        assert_eq!(series.total_samples(), 10);

        // Before transition: must return None
        assert_eq!(series.get_state_at(49_999), None);
        assert_eq!(series.get_state_at(0), None);

        // Exact transition timestamp: last state in sequence wins under ZOH
        assert_eq!(series.get_state_at(50_000), Some(10));

        // Far after transition timestamp: holds last state
        assert_eq!(series.get_state_at(50_001), Some(10));
        assert_eq!(series.get_state_at(1_000_000), Some(10));

        // Index inspection: each discrete transition is accurately preserved
        for i in 0..10i32 {
            assert_eq!(
                series.get_transition_by_index(i as u64),
                Some((50_000, i + 1))
            );
        }
        assert_eq!(series.get_transition_by_index(10), None);
    }

    #[test]
    fn test_simultaneous_events_across_consecutive_batches() {
        let mut series = EventSeries::new(ChannelId(2));

        // Batch 1 ends at ts = 100_000 with state 42
        series.append(&[90_000, 100_000], &[10, 42]).unwrap();
        assert_eq!(series.get_state_at(100_000), Some(42));

        // Batch 2 immediately starts at ts = 100_000 with state 99
        series.append(&[100_000, 100_000, 120_000], &[88, 99, 105]).unwrap();

        // At ts = 100_000, the last state appended at that timestamp was 99
        assert_eq!(series.get_state_at(100_000), Some(99));
        assert_eq!(series.get_state_at(110_000), Some(99));
        assert_eq!(series.get_state_at(120_000), Some(105));
        assert_eq!(series.get_state_at(99_999), Some(10));

        assert_eq!(series.total_samples(), 5);
        assert_eq!(series.get_transition_by_index(0), Some((90_000, 10)));
        assert_eq!(series.get_transition_by_index(1), Some((100_000, 42)));
        assert_eq!(series.get_transition_by_index(2), Some((100_000, 88)));
        assert_eq!(series.get_transition_by_index(3), Some((100_000, 99)));
        assert_eq!(series.get_transition_by_index(4), Some((120_000, 105)));
    }

    #[test]
    fn test_simultaneous_events_spanning_chunk_boundary_65536() {
        let mut series = EventSeries::new(ChannelId(3));

        // Exactly 65,536 events all at ts = 1_000_000, stepping states 0..65536
        // This triggers automatic chunk sealing at the 65,536th event.
        let ts1 = vec![1_000_000u64; CHUNK_LEN];
        let states1: Vec<i32> = (0..CHUNK_LEN as i32).collect();

        series.append(&ts1, &states1).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 0);
        assert_eq!(series.total_samples(), CHUNK_LEN as u64);

        // Append 5 more events, STILL at ts = 1_000_000, states 70000..70005 into active chunk
        let ts2 = vec![1_000_000u64; 5];
        let states2: Vec<i32> = (70_000..70_005).collect();
        series.append(&ts2, &states2).unwrap();

        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 5);
        assert_eq!(series.total_samples(), (CHUNK_LEN + 5) as u64);

        // Before timestamp: None
        assert_eq!(series.get_state_at(999_999), None);

        // At exact timestamp: latest state in active chunk wins (70004)
        assert_eq!(series.get_state_at(1_000_000), Some(70_004));

        // After timestamp: holds 70004
        assert_eq!(series.get_state_at(2_000_000), Some(70_004));

        // Seal active chunk explicitly
        let handle = series.seal_active().unwrap().expect("active chunk has 5 samples");
        assert_eq!(handle.sample_count, 5);
        assert_eq!(series.sealed_chunk_count(), 2);
        assert_eq!(series.active_chunk().len(), 0);

        // Re-query: results must remain strictly identical
        assert_eq!(series.get_state_at(999_999), None);
        assert_eq!(series.get_state_at(1_000_000), Some(70_004));
        assert_eq!(series.get_state_at(2_000_000), Some(70_004));

        // Index retrieval across sealed chunk boundary
        assert_eq!(
            series.get_transition_by_index(CHUNK_LEN as u64 - 1),
            Some((1_000_000, CHUNK_LEN as i32 - 1))
        );
        assert_eq!(
            series.get_transition_by_index(CHUNK_LEN as u64),
            Some((1_000_000, 70_000))
        );
        assert_eq!(
            series.get_transition_by_index(CHUNK_LEN as u64 + 4),
            Some((1_000_000, 70_004))
        );
        assert_eq!(series.get_transition_by_index(CHUNK_LEN as u64 + 5), None);
    }

    #[test]
    fn test_rapid_oscillations_100k_crossing_chunk_boundary() {
        let mut series = EventSeries::new(ChannelId(4));
        let count = 100_000usize;
        let step_ns = 10u64;
        let start_ts = 50_000u64;

        let timestamps: Vec<TimestampNs> = (0..count).map(|i| start_ts + (i as u64) * step_ns).collect();
        let states: Vec<i32> = (0..count).map(|i| (i % 2) as i32).collect();

        // Append in three unequal chunks: 40,000 + 40,000 + 20,000
        assert_eq!(series.append(&timestamps[0..40_000], &states[0..40_000]).unwrap(), 40_000);
        assert_eq!(series.sealed_chunk_count(), 0);

        // Second batch crosses 65,536 boundary (40,000 + 40,000 = 80,000 -> 1 sealed chunk of 65,536)
        assert_eq!(series.append(&timestamps[40_000..80_000], &states[40_000..80_000]).unwrap(), 40_000);
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 80_000 - CHUNK_LEN);

        // Third batch completes 100,000
        assert_eq!(series.append(&timestamps[80_000..100_000], &states[80_000..100_000]).unwrap(), 20_000);
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.total_samples(), 100_000);

        // 1. Prior to first event
        assert_eq!(series.get_state_at(start_ts - 1), None);
        assert_eq!(series.get_state_at(0), None);

        // 2. Exact probe points across Chunk 0 and Active Chunk
        let probe_indices = [
            0, 1, 2, 100, 5000, 40_000, 65_534, 65_535, 65_536, 65_537, 79_999, 80_000, 99_998, 99_999,
        ];
        for &idx in &probe_indices {
            let t = timestamps[idx];
            let expected_state = (idx % 2) as i32;

            // Exact transition point
            assert_eq!(
                series.get_state_at(t),
                Some(expected_state),
                "Failed at index {idx}, exact ts {t}"
            );

            // Right after transition (before next)
            assert_eq!(
                series.get_state_at(t + 1),
                Some(expected_state),
                "Failed at index {idx}, ts + 1 ({})",
                t + 1
            );
            assert_eq!(
                series.get_state_at(t + step_ns - 1),
                Some(expected_state),
                "Failed at index {idx}, ts + step - 1 ({})",
                t + step_ns - 1
            );

            // Right before transition (except first index)
            if idx > 0 {
                let prev_expected = ((idx - 1) % 2) as i32;
                assert_eq!(
                    series.get_state_at(t - 1),
                    Some(prev_expected),
                    "Failed at index {idx}, ts - 1 ({})",
                    t - 1
                );
            }
        }

        // 3. Far beyond last event
        let last_ts = *timestamps.last().unwrap();
        let last_state = *states.last().unwrap();
        assert_eq!(series.get_state_at(last_ts + 1_000_000_000), Some(last_state));
    }

    #[test]
    fn test_zoh_comprehensive_matrix_across_sealing_states() {
        // Build 4 identical event series under 4 different chunking/sealing topologies:
        // Config A: Entirely in active buffer
        // Config B: Sealed halfway
        // Config C: Fully sealed into 1 chunk
        // Config D: Explicitly sealed after EVERY batch into micro-chunks
        let raw_events: &[(TimestampNs, i32)] = &[
            (100, 10),
            (250, 20),
            (250, 25), // simultaneous
            (400, 30),
            (1000, 40),
            (1000, 41), // simultaneous
            (1000, 42), // simultaneous
            (5000, 50),
            (10000, 60),
        ];

        let ts: Vec<TimestampNs> = raw_events.iter().map(|(t, _)| *t).collect();
        let codes: Vec<i32> = raw_events.iter().map(|(_, c)| *c).collect();

        // Config A: Active only
        let mut series_a = EventSeries::new(ChannelId(10));
        series_a.append(&ts, &codes).unwrap();

        // Config B: Half sealed
        let mut series_b = EventSeries::new(ChannelId(11));
        series_b.append(&ts[..4], &codes[..4]).unwrap();
        series_b.seal_active().unwrap();
        series_b.append(&ts[4..], &codes[4..]).unwrap();

        // Config C: Fully sealed
        let mut series_c = EventSeries::new(ChannelId(12));
        series_c.append(&ts, &codes).unwrap();
        series_c.seal_active().unwrap();

        // Config D: Micro-chunks
        let mut series_d = EventSeries::new(ChannelId(13));
        for i in 0..ts.len() {
            series_d.append(&ts[i..=i], &codes[i..=i]).unwrap();
            series_d.seal_active().unwrap();
        }

        let probe_points: &[(TimestampNs, Option<i32>)] = &[
            (0, None),
            (50, None),
            (99, None),
            (100, Some(10)),
            (101, Some(10)),
            (249, Some(10)),
            (250, Some(25)),
            (251, Some(25)),
            (399, Some(25)),
            (400, Some(30)),
            (401, Some(30)),
            (999, Some(30)),
            (1000, Some(42)),
            (1001, Some(42)),
            (4999, Some(42)),
            (5000, Some(50)),
            (5001, Some(50)),
            (9999, Some(50)),
            (10000, Some(60)),
            (10001, Some(60)),
            (99999999, Some(60)),
        ];

        for &(probe_ts, expected) in probe_points {
            assert_eq!(
                series_a.get_state_at(probe_ts),
                expected,
                "Config A mismatch at {probe_ts}"
            );
            assert_eq!(
                series_b.get_state_at(probe_ts),
                expected,
                "Config B mismatch at {probe_ts}"
            );
            assert_eq!(
                series_c.get_state_at(probe_ts),
                expected,
                "Config C mismatch at {probe_ts}"
            );
            assert_eq!(
                series_d.get_state_at(probe_ts),
                expected,
                "Config D mismatch at {probe_ts}"
            );
        }
    }

    #[test]
    fn test_extreme_state_codes() {
        let mut series = EventSeries::new(ChannelId(5));
        let ts = [10, 20, 30, 40, 50];
        let states = [i32::MIN, -1, 0, 1, i32::MAX];

        series.append(&ts, &states).unwrap();
        assert_eq!(series.get_state_at(5), None);
        assert_eq!(series.get_state_at(10), Some(i32::MIN));
        assert_eq!(series.get_state_at(15), Some(i32::MIN));
        assert_eq!(series.get_state_at(20), Some(-1));
        assert_eq!(series.get_state_at(30), Some(0));
        assert_eq!(series.get_state_at(40), Some(1));
        assert_eq!(series.get_state_at(50), Some(i32::MAX));
        assert_eq!(series.get_state_at(100), Some(i32::MAX));
    }

    #[test]
    fn test_extreme_timestamps() {
        let mut series = EventSeries::new(ChannelId(6));
        let base_ts = u64::MAX - 100;
        let ts = [base_ts, base_ts + 10, base_ts + 20];
        let states = [100, 200, 300];

        series.append(&ts, &states).unwrap();
        assert_eq!(series.get_state_at(base_ts - 1), None);
        assert_eq!(series.get_state_at(base_ts), Some(100));
        assert_eq!(series.get_state_at(base_ts + 5), Some(100));
        assert_eq!(series.get_state_at(base_ts + 10), Some(200));
        assert_eq!(series.get_state_at(base_ts + 20), Some(300));
        assert_eq!(series.get_state_at(u64::MAX), Some(300));
    }

    #[test]
    fn test_transactional_rollback_on_hostile_invalid_batches() {
        let mut series = EventSeries::new(ChannelId(7));
        series.append(&[100, 200], &[1, 2]).unwrap();
        assert_eq!(series.total_samples(), 2);

        // 1. Retrograde within batch
        let err1 = series.append(&[300, 250], &[3, 4]);
        assert!(matches!(err1, Err(CoreError::Storage(_))));
        assert_eq!(series.total_samples(), 2);
        assert_eq!(series.latest_timestamp(), Some(200));

        // 2. Retrograde against series head
        let err2 = series.append(&[150, 250], &[5, 6]);
        assert!(matches!(err2, Err(CoreError::Storage(_))));
        assert_eq!(series.total_samples(), 2);

        // 3. Length mismatch
        let err3 = series.append(&[300, 400], &[7]);
        assert!(matches!(err3, Err(CoreError::Storage(_))));
        assert_eq!(series.total_samples(), 2);

        // 4. Submitting a valid batch succeeds immediately afterwards
        assert_eq!(series.append(&[200, 300], &[10, 20]).unwrap(), 2);
        assert_eq!(series.total_samples(), 4);
        assert_eq!(series.get_state_at(200), Some(10));
        assert_eq!(series.get_state_at(300), Some(20));
    }
}

// ============================================================================
// Module 2: Watermark Engine Stress
// ============================================================================
mod watermark_engine_stress {
    use super::*;

    #[test]
    fn test_empty_batch_silence_verification() {
        let mut series = EventSeries::new(ChannelId(20));
        let listener = StressWatermarkListener::new();
        series.register_listener(listener.clone());

        // Repeated empty appends must never dispatch events
        for _ in 0..100 {
            assert_eq!(series.append(&[], &[]).unwrap(), 0);
        }
        assert_eq!(listener.total_calls(), 0);
        assert_eq!(listener.total_event_records(), 0);

        // Explicit seal on empty series emits no watermark events
        assert!(series.seal_active().unwrap().is_none());
        assert_eq!(listener.total_calls(), 0);

        // Non-empty append triggers exactly 1 event
        series.append(&[100], &[1]).unwrap();
        assert_eq!(listener.total_calls(), 1);
        assert_eq!(listener.total_event_records(), 1);

        // Subsequent empty appends remain completely silent
        for _ in 0..50 {
            assert_eq!(series.append(&[], &[]).unwrap(), 0);
        }
        assert_eq!(listener.total_calls(), 1);
        assert_eq!(listener.total_event_records(), 1);
    }

    #[test]
    fn test_single_series_rapid_thousands_commits() {
        let mut series = EventSeries::new(ChannelId(21));
        let listener = StressWatermarkListener::new();
        series.register_listener(listener.clone());

        let commit_count = 3_000usize;
        let mut expected_samples = 0u64;
        let mut current_ts = 1_000u64;

        for commit_idx in 0..commit_count {
            let batch_len = (commit_idx % 15) + 1;
            let ts: Vec<TimestampNs> = (0..batch_len)
                .map(|i| {
                    current_ts += i as u64;
                    current_ts
                })
                .collect();
            let states: Vec<i32> = (0..batch_len).map(|i| (i + commit_idx) as i32).collect();

            series.append(&ts, &states).unwrap();
            expected_samples += batch_len as u64;
        }

        assert_eq!(series.total_samples(), expected_samples);
        assert_eq!(listener.total_calls(), commit_count);
        assert_eq!(listener.channel_event_count(ChannelId(21)), commit_count);

        let events = listener.channel_events(ChannelId(21));
        let mut prev_ts = 0u64;
        let mut prev_samples = 0u64;

        for (idx, ev) in events.iter().enumerate() {
            assert_eq!(ev.channel_id, ChannelId(21));
            assert!(
                ev.latest_ts_ns >= prev_ts,
                "Event {idx}: non-monotonic timestamp {} < {}",
                ev.latest_ts_ns,
                prev_ts
            );
            assert!(
                ev.total_samples > prev_samples,
                "Event {idx}: sample count did not increase: {} <= {}",
                ev.total_samples,
                prev_samples
            );
            prev_ts = ev.latest_ts_ns;
            prev_samples = ev.total_samples;
        }

        assert_eq!(prev_samples, expected_samples);
    }

    #[test]
    fn test_multi_series_multi_listener_fanout_stress() {
        let num_series = 6usize;
        let mut series_vec: Vec<EventSeries> = (0..num_series)
            .map(|i| EventSeries::new(ChannelId((100 + i) as u32)))
            .collect();

        // 4 distinct listeners:
        // Listener All: attached to all 6 series
        // Listener Even: attached to channels with even index (100, 102, 104)
        // Listener Odd: attached to channels with odd index (101, 103, 105)
        // Listener Single: attached only to channel 100
        let listener_all = StressWatermarkListener::new();
        let listener_even = StressWatermarkListener::new();
        let listener_odd = StressWatermarkListener::new();
        let listener_single = StressWatermarkListener::new();

        for (idx, s) in series_vec.iter_mut().enumerate() {
            s.register_listener(listener_all.clone());
            if idx % 2 == 0 {
                s.register_listener(listener_even.clone());
            } else {
                s.register_listener(listener_odd.clone());
            }
            if idx == 0 {
                s.register_listener(listener_single.clone());
            }
        }

        let commits_per_series = 500usize;
        let mut channel_expected_samples: HashMap<ChannelId, u64> = HashMap::new();
        let mut ts_cursor = 10_000u64;

        for commit_idx in 0..commits_per_series {
            for (idx, s) in series_vec.iter_mut().enumerate() {
                // Interleave 10% empty commits
                if (commit_idx + idx) % 10 == 0 {
                    s.append(&[], &[]).unwrap();
                    continue;
                }

                let batch_len = (commit_idx % 8) + 1;
                let ts: Vec<TimestampNs> = (0..batch_len)
                    .map(|_| {
                        ts_cursor += 5;
                        ts_cursor
                    })
                    .collect();
                let states: Vec<i32> = (0..batch_len).map(|_| idx as i32).collect();

                s.append(&ts, &states).unwrap();
                *channel_expected_samples.entry(s.channel_id()).or_default() += batch_len as u64;
            }
        }

        // Verify Listener All
        let total_all_records = listener_all.total_event_records();
        let sum_channel_events: usize = (0..num_series)
            .map(|i| listener_all.channel_event_count(ChannelId((100 + i) as u32)))
            .sum();
        assert_eq!(total_all_records, sum_channel_events);

        // Verify Listener Even received 0 events for odd channels
        for i in 0..num_series {
            let cid = ChannelId((100 + i) as u32);
            if i % 2 == 0 {
                assert!(listener_even.channel_event_count(cid) > 0);
            } else {
                assert_eq!(listener_even.channel_event_count(cid), 0);
            }
        }

        // Verify Listener Odd received 0 events for even channels
        for i in 0..num_series {
            let cid = ChannelId((100 + i) as u32);
            if i % 2 != 0 {
                assert!(listener_odd.channel_event_count(cid) > 0);
            } else {
                assert_eq!(listener_odd.channel_event_count(cid), 0);
            }
        }

        // Verify Listener Single received events ONLY from Channel 100
        assert_eq!(
            listener_single.channel_event_count(ChannelId(100)),
            listener_all.channel_event_count(ChannelId(100))
        );
        for i in 1..num_series {
            assert_eq!(listener_single.channel_event_count(ChannelId((100 + i) as u32)), 0);
        }

        // Verify per-channel strictly monotonic advancement on all channels in listener_all
        for i in 0..num_series {
            let cid = ChannelId((100 + i) as u32);
            let events = listener_all.channel_events(cid);
            let mut prev_ts = 0u64;
            let mut prev_tot = 0u64;
            for ev in &events {
                assert!(ev.latest_ts_ns >= prev_ts);
                assert!(ev.total_samples > prev_tot);
                prev_ts = ev.latest_ts_ns;
                prev_tot = ev.total_samples;
            }
            assert_eq!(prev_tot, channel_expected_samples[&cid]);
        }
    }

    #[test]
    fn test_multithreaded_concurrent_listener_invocations() {
        let num_threads = 6usize;
        let commits_per_thread = 800usize;
        let shared_listener = StressWatermarkListener::new();

        std::thread::scope(|s| {
            for thread_idx in 0..num_threads {
                let listener_ref = shared_listener.clone();
                s.spawn(move || {
                    let channel_id = ChannelId((500 + thread_idx) as u32);
                    let mut series = EventSeries::new(channel_id);
                    series.register_listener(listener_ref);

                    let mut ts = 1_000_000u64 + (thread_idx as u64) * 100_000;
                    for c in 0..commits_per_thread {
                        if c % 12 == 0 {
                            series.append(&[], &[]).unwrap();
                            continue;
                        }

                        let len = (c % 10) + 1;
                        let ts_batch: Vec<TimestampNs> = (0..len)
                            .map(|_| {
                                ts += 2;
                                ts
                            })
                            .collect();
                        let states_batch: Vec<i32> = (0..len).map(|i| (i + c) as i32).collect();

                        series.append(&ts_batch, &states_batch).unwrap();
                    }
                });
            }
        });

        // Verify no events were dropped and per-channel monotonicity was preserved under concurrent writes
        for thread_idx in 0..num_threads {
            let cid = ChannelId((500 + thread_idx) as u32);
            let events = shared_listener.channel_events(cid);
            assert!(!events.is_empty(), "Thread {thread_idx} produced zero events");

            let mut prev_ts = 0u64;
            let mut prev_samples = 0u64;
            for ev in &events {
                assert_eq!(ev.channel_id, cid);
                assert!(ev.latest_ts_ns >= prev_ts);
                assert!(ev.total_samples > prev_samples);
                prev_ts = ev.latest_ts_ns;
                prev_samples = ev.total_samples;
            }
        }
    }

    #[test]
    fn test_large_batch_multi_boundary_rollover_single_watermark() {
        let mut series = EventSeries::new(ChannelId(30));
        let listener = StressWatermarkListener::new();
        series.register_listener(listener.clone());

        // Single batch of 200,000 samples spans across 3 chunk boundaries (65,536 each)
        let total = 200_000usize;
        let ts: Vec<TimestampNs> = (0..total).map(|i| (i as u64) * 10).collect();
        let states: Vec<i32> = (0..total).map(|i| (i % 5) as i32).collect();

        assert_eq!(series.append(&ts, &states).unwrap(), total);
        assert_eq!(series.sealed_chunk_count(), 3);
        assert_eq!(series.total_samples(), total as u64);

        // Invariant: exactly 1 watermark event emitted per append call
        assert_eq!(listener.total_calls(), 1);
        assert_eq!(listener.channel_event_count(ChannelId(30)), 1);

        let ev = listener.channel_events(ChannelId(30))[0];
        assert_eq!(ev.channel_id, ChannelId(30));
        assert_eq!(ev.latest_ts_ns, *ts.last().unwrap());
        assert_eq!(ev.total_samples, total as u64);
    }
}

// ============================================================================
// Module 3: Sealing & Interleaving
// ============================================================================
mod sealing_and_interleaving {
    use super::*;

    #[test]
    fn test_micro_chunks_explicit_seal_interleaving() {
        let mut series = EventSeries::new(ChannelId(40));

        // Create 50 micro-chunks of 1 to 5 samples each
        let mut expected_transitions: Vec<(TimestampNs, i32)> = Vec::new();
        let mut cur_ts = 100u64;

        for chunk_idx in 0..50 {
            let len = (chunk_idx % 5) + 1;
            let ts: Vec<TimestampNs> = (0..len)
                .map(|_| {
                    cur_ts += 10;
                    cur_ts
                })
                .collect();
            let states: Vec<i32> = (0..len).map(|i| (chunk_idx * 10 + i) as i32).collect();

            series.append(&ts, &states).unwrap();
            for (&t, &c) in ts.iter().zip(states.iter()) {
                expected_transitions.push((t, c));
            }

            // Seal explicitly
            let handle = series.seal_active().unwrap().expect("micro-chunk non-empty");
            assert_eq!(handle.sample_count, len as u32);
            assert_eq!(series.sealed_chunk_count(), chunk_idx + 1);
        }

        assert_eq!(series.total_samples(), expected_transitions.len() as u64);
        assert_eq!(series.active_chunk().len(), 0);

        // Verify index queries across all 50 micro-chunks
        for (idx, &(exp_ts, exp_code)) in expected_transitions.iter().enumerate() {
            assert_eq!(
                series.get_transition_by_index(idx as u64),
                Some((exp_ts, exp_code)),
                "Failed at micro-chunk index {idx}"
            );
        }
        assert_eq!(series.get_transition_by_index(expected_transitions.len() as u64), None);

        // Verify zero-order hold state at each transition point and between
        for &(t, exp_code) in &expected_transitions {
            assert_eq!(series.get_state_at(t), Some(exp_code));
            assert_eq!(series.get_state_at(t + 1), Some(exp_code));
        }
    }

    #[test]
    fn test_idempotent_empty_seals() {
        let mut series = EventSeries::new(ChannelId(41));

        // Calling seal_active on empty series returns Ok(None) repeatedly
        for _ in 0..10 {
            assert!(series.seal_active().unwrap().is_none());
            assert_eq!(series.sealed_chunk_count(), 0);
        }

        // Add 3 samples and seal
        series.append(&[10, 20, 30], &[1, 2, 3]).unwrap();
        assert!(series.seal_active().unwrap().is_some());
        assert_eq!(series.sealed_chunk_count(), 1);

        // Calling seal_active again on newly emptied buffer returns Ok(None) repeatedly
        for _ in 0..10 {
            assert!(series.seal_active().unwrap().is_none());
            assert_eq!(series.sealed_chunk_count(), 1);
        }
    }

    #[test]
    fn test_mixed_auto_seal_and_manual_seal() {
        let mut series = EventSeries::new(ChannelId(42));

        // Phase 1: Append 65,536 samples (auto-sealed -> chunk 0)
        let ts1 = vec![1000u64; CHUNK_LEN];
        let states1 = vec![1i32; CHUNK_LEN];
        series.append(&ts1, &states1).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 0);

        // Phase 2: Append 10 samples (in active buffer)
        series.append(&[2000; 10], &[2; 10]).unwrap();
        assert_eq!(series.sealed_chunk_count(), 1);
        assert_eq!(series.active_chunk().len(), 10);

        // Manual seal active -> chunk 1
        assert!(series.seal_active().unwrap().is_some());
        assert_eq!(series.sealed_chunk_count(), 2);
        assert_eq!(series.active_chunk().len(), 0);

        // Phase 3: Append 70,000 samples (auto-seals 65,536 -> chunk 2, leaves 4,464 in active)
        let ts3 = vec![3000u64; 70_000];
        let states3 = vec![3i32; 70_000];
        series.append(&ts3, &states3).unwrap();
        assert_eq!(series.sealed_chunk_count(), 3);
        assert_eq!(series.active_chunk().len(), 4_464);

        // Manual seal active -> chunk 3
        assert!(series.seal_active().unwrap().is_some());
        assert_eq!(series.sealed_chunk_count(), 4);
        assert_eq!(series.active_chunk().len(), 0);

        assert_eq!(series.total_samples(), (CHUNK_LEN + 10 + 70_000) as u64);

        // Index checks across all 4 chunks
        assert_eq!(series.get_transition_by_index(0), Some((1000, 1)));
        assert_eq!(series.get_transition_by_index(CHUNK_LEN as u64 - 1), Some((1000, 1)));
        assert_eq!(series.get_transition_by_index(CHUNK_LEN as u64), Some((2000, 2)));
        assert_eq!(series.get_transition_by_index(CHUNK_LEN as u64 + 9), Some((2000, 2)));
        assert_eq!(series.get_transition_by_index(CHUNK_LEN as u64 + 10), Some((3000, 3)));
        assert_eq!(series.get_transition_by_index(series.total_samples() - 1), Some((3000, 3)));
        assert_eq!(series.get_transition_by_index(series.total_samples()), None);
    }

    #[test]
    fn test_randomized_fuzz_interleaving_golden_oracle() {
        let mut rng = FuzzRng::new(0xDEADBEEFCAFEBABE);
        let mut series = EventSeries::new(ChannelId(43));
        let listener = StressWatermarkListener::new();
        series.register_listener(listener.clone());

        let mut golden: Vec<(TimestampNs, i32)> = Vec::new();
        let mut current_ts = 100u64;
        let mut expected_commits = 0usize;

        let iterations = 2_000usize;
        for _ in 0..iterations {
            let action = rng.next_range(1, 6);
            match action {
                1 | 2 => {
                    // Action 1/2: Valid batch append (varying from 1 to 300 samples)
                    let batch_len = rng.next_range(1, 300);
                    let mut ts_batch = Vec::with_capacity(batch_len);
                    let mut code_batch = Vec::with_capacity(batch_len);

                    for _ in 0..batch_len {
                        // 15% probability of simultaneous event (same timestamp)
                        if !rng.next_bool(15) {
                            current_ts += rng.next_range(1, 20) as u64;
                        }
                        let code = rng.next_i32();
                        ts_batch.push(current_ts);
                        code_batch.push(code);
                        golden.push((current_ts, code));
                    }

                    series.append(&ts_batch, &code_batch).unwrap();
                    expected_commits += 1;
                }
                3 => {
                    // Action 3: Empty batch append
                    series.append(&[], &[]).unwrap();
                }
                4 => {
                    // Action 4: Hostile retrograde append (must fail and leave state pristine)
                    if let Some(&(last_ts, _)) = golden.last() {
                        if last_ts > 10 {
                            let bad_ts = [last_ts - 5];
                            let bad_code = [999];
                            let res = series.append(&bad_ts, &bad_code);
                            assert!(res.is_err());
                        }
                    }
                }
                5 => {
                    // Action 5: Explicit seal_active
                    let _ = series.seal_active();
                }
                6 => {
                    // Action 6: State and Index Verification
                    if golden.is_empty() {
                        assert_eq!(series.get_state_at(50), None);
                        assert_eq!(series.get_transition_by_index(0), None);
                    } else {
                        // Query index
                        let q_idx = rng.next_range(0, golden.len() - 1);
                        assert_eq!(
                            series.get_transition_by_index(q_idx as u64),
                            Some(golden[q_idx])
                        );

                        // Query boundary timestamp
                        let sample = golden[q_idx];
                        let golden_zoh_state = {
                            let p = golden.partition_point(|&(t, _)| t <= sample.0);
                            golden[p - 1].1
                        };
                        assert_eq!(series.get_state_at(sample.0), Some(golden_zoh_state));
                    }
                }
                _ => unreachable!(),
            }
        }

        // Final thorough audit against golden oracle
        assert_eq!(series.total_samples(), golden.len() as u64);
        assert_eq!(listener.channel_event_count(ChannelId(43)), expected_commits);

        // Verify index lookups
        for i in (0..golden.len()).step_by(17) {
            assert_eq!(series.get_transition_by_index(i as u64), Some(golden[i]));
        }
        assert_eq!(series.get_transition_by_index(golden.len() as u64), None);

        // Verify timestamps before first and far after last
        if let Some(&(first_ts, _)) = golden.first() {
            if first_ts > 0 {
                assert_eq!(series.get_state_at(first_ts - 1), None);
            }
        }
        if let Some(&(last_ts, _)) = golden.last() {
            let last_state = golden.last().unwrap().1;
            assert_eq!(series.get_state_at(last_ts + 1_000_000), Some(last_state));
        }
    }
}

// ============================================================================
// Module 4: Series Lifecycle & Invariants
// ============================================================================
mod series_lifecycle_and_invariants {
    use super::*;

    #[test]
    fn test_clone_independence_and_shared_sealed_data() {
        let mut series1 = EventSeries::new(ChannelId(50));
        series1.append(&[100, 200, 300], &[10, 20, 30]).unwrap();
        series1.seal_active().unwrap();
        series1.append(&[400, 500], &[40, 50]).unwrap();

        // Clone series1
        let mut series2 = series1.clone();
        assert_eq!(series2.channel_id(), ChannelId(50));
        assert_eq!(series2.total_samples(), 5);
        assert_eq!(series2.sealed_chunk_count(), 1);

        // Append to series2 independently
        series2.append(&[600, 700], &[60, 70]).unwrap();
        assert_eq!(series2.total_samples(), 7);
        assert_eq!(series1.total_samples(), 5); // series1 unmodified

        // Verify series1 states
        assert_eq!(series1.get_state_at(500), Some(50));
        assert_eq!(series1.get_state_at(600), Some(50)); // held
        assert_eq!(series1.get_transition_by_index(5), None);

        // Verify series2 states
        assert_eq!(series2.get_state_at(500), Some(50));
        assert_eq!(series2.get_state_at(600), Some(60));
        assert_eq!(series2.get_state_at(700), Some(70));
        assert_eq!(series2.get_transition_by_index(5), Some((600, 60)));
        assert_eq!(series2.get_transition_by_index(6), Some((700, 70)));
    }

    #[test]
    fn test_first_latest_time_range_lifecycle() {
        let mut series = EventSeries::new(ChannelId(51));
        assert_eq!(series.first_timestamp(), None);
        assert_eq!(series.latest_timestamp(), None);
        assert_eq!(series.time_range_ns(), None);
        assert!(series.is_empty());

        // Stage 1: Active buffer only
        series.append(&[1_000, 2_000], &[1, 2]).unwrap();
        assert_eq!(series.first_timestamp(), Some(1_000));
        assert_eq!(series.latest_timestamp(), Some(2_000));
        assert_eq!(series.time_range_ns(), Some((1_000, 2_000)));
        assert!(!series.is_empty());

        // Stage 2: Sealed only (active buffer empty)
        series.seal_active().unwrap();
        assert_eq!(series.first_timestamp(), Some(1_000));
        assert_eq!(series.latest_timestamp(), Some(2_000));
        assert_eq!(series.time_range_ns(), Some((1_000, 2_000)));

        // Stage 3: Sealed + Active buffer
        series.append(&[3_000, 5_000], &[3, 4]).unwrap();
        assert_eq!(series.first_timestamp(), Some(1_000));
        assert_eq!(series.latest_timestamp(), Some(5_000));
        assert_eq!(series.time_range_ns(), Some((1_000, 5_000)));
    }
}
