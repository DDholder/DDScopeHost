use ddscope_core::clock::{
    ClockMode, ClockSyncConfig, CounterUnwrapper, LinearDriftTracker, StreamClockSynchronizer,
    DRIFT_SLOPE_MAX, DRIFT_SLOPE_MIN,
};
use ddscope_core::types::{
    ChannelDescriptor, CoreError, DecodedBatch, ScalarType, TimestampNs,
};

/// Deterministic SplitMix64 PRNG for reproducible jitter tests without extra dependencies.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Returns a deterministic jitter value uniformly distributed in `[-half_span_ns, +half_span_ns]`.
    fn next_signed_ns(&mut self, half_span_ns: i64) -> i64 {
        let u = (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64); // [0.0, 1.0)
        ((2.0 * u - 1.0) * (half_span_ns as f64)).round() as i64
    }
}

fn interval_stats(timestamps: &[TimestampNs]) -> (f64, f64, f64, f64) {
    assert!(timestamps.len() >= 2);
    let n = (timestamps.len() - 1) as f64;
    let mut sum = 0.0_f64;
    for w in timestamps.windows(2) {
        assert!(
            w[1] >= w[0],
            "non-monotonic timestamps detected: {} -> {}",
            w[0],
            w[1]
        );
        sum += (w[1] - w[0]) as f64;
    }
    let mean = sum / n;
    let mut sq_sum = 0.0_f64;
    for w in timestamps.windows(2) {
        let d = (w[1] - w[0]) as f64 - mean;
        sq_sum += d * d;
    }
    let var = sq_sum / n;
    let std_dev = var.sqrt();
    let rel_std = std_dev / mean;
    let norm_var = var / (mean * mean);
    (mean, std_dev, rel_std, norm_var)
}

#[test]
fn clock_mode_serde_and_config_validation() {
    let modes = [
        ClockMode::Tier1HardwareCounter {
            tick_hz: 1_000_000.0,
            counter_bits: 32,
        },
        ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(2_000_000.0),
        },
        ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz: None,
        },
        ClockMode::Tier3HostArrival,
    ];
    for mode in modes {
        let json = serde_json::to_string(&mode).expect("serialize ClockMode");
        let parsed: ClockMode = serde_json::from_str(&json).expect("deserialize ClockMode");
        assert_eq!(parsed, mode);
    }
    assert_eq!(ClockMode::default(), ClockMode::Tier3HostArrival);

    // Invalid Tier 1 configs
    for bad_hz in [0.0, -1_000.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let res = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
            tick_hz: bad_hz,
            counter_bits: 32,
        });
        assert!(matches!(res, Err(CoreError::InvalidConfig(_))));
    }
    for bad_bits in [0, 7, 65, 128] {
        let res = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
            tick_hz: 1_000_000.0,
            counter_bits: bad_bits,
        });
        assert!(matches!(res, Err(CoreError::InvalidConfig(_))));
    }
    for valid_bits in [8, 16, 24, 32, 64] {
        let sync = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
            tick_hz: 1_000_000.0,
            counter_bits: valid_bits,
        })
        .expect("valid Tier1 bits");
        assert_eq!(
            sync.mode(),
            ClockMode::Tier1HardwareCounter {
                tick_hz: 1_000_000.0,
                counter_bits: valid_bits,
            }
        );
    }

    // Invalid Tier 2 configs
    for bad_fs in [0.0, -500.0, f64::NAN, f64::INFINITY] {
        let res = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(bad_fs),
        });
        assert!(matches!(res, Err(CoreError::InvalidConfig(_))));
    }

    // Invalid ClockSyncConfig values
    let bad_cfg = ClockSyncConfig {
        drift_window_size: 1,
        ..ClockSyncConfig::default()
    };
    assert!(matches!(
        StreamClockSynchronizer::with_config(
            ClockMode::Tier1HardwareCounter {
                tick_hz: 1_000_000.0,
                counter_bits: 16,
            },
            bad_cfg,
        ),
        Err(CoreError::InvalidConfig(_))
    ));

    let custom_cfg = ClockSyncConfig {
        drift_window_size: 32,
        drift_ewma_alpha: 0.25,
        pll_kp: 0.002,
        pll_ki: 2.0e-6,
        pll_max_freq_deviation_ratio: 0.02,
        adaptive_window_ns: 40_000_000,
        gap_reset_ns: 150_000_000,
    };
    let mut custom_sync = StreamClockSynchronizer::with_config(
        ClockMode::Tier2SoftwarePll {
            nominal_sample_rate_hz: Some(1_000_000.0),
        },
        custom_cfg,
    )
    .unwrap();
    let mut small_out = [0u64; 2];
    assert!(matches!(
        custom_sync.assign_timestamps(1_000, None, 4, &mut small_out),
        Err(CoreError::Clock(_))
    ));
    custom_sync.assign_timestamps(1_000, None, 0, &mut small_out).unwrap();
}

#[test]
fn tier1_counter_unwrapper_handles_16_24_32_64_bit_rollovers() {
    // 16-bit rollover across multiple boundaries + dirty upper bits masked out
    let mut u16_unwrap = CounterUnwrapper::new(1_000_000.0, 16).unwrap();
    assert_eq!(u16_unwrap.push_raw(65_530), 0.0);
    assert_eq!(u16_unwrap.push_raw(5 | (0xDEAD_0000)), 11_000.0);
    assert_eq!(u16_unwrap.push_raw(65_535), 11_000.0 + 65_530_000.0);
    assert_eq!(u16_unwrap.push_raw(0), 11_000.0 + 65_531_000.0);
    assert_eq!(u16_unwrap.unwrapped_ticks(), 65_542);

    // 24-bit rollover
    let mut u24_unwrap = CounterUnwrapper::new(1_000_000.0, 24).unwrap();
    assert_eq!(u24_unwrap.push_raw(16_777_210), 0.0);
    assert_eq!(u24_unwrap.push_raw(5), 11_000.0);
    assert_eq!(u24_unwrap.unwrapped_ticks(), 11);

    // 32-bit rollover
    let mut u32_unwrap = CounterUnwrapper::new(1_000_000.0, 32).unwrap();
    assert_eq!(u32_unwrap.push_raw(u64::from(u32::MAX - 9)), 0.0);
    assert_eq!(u32_unwrap.push_raw(10), 20_000.0);
    assert_eq!(u32_unwrap.unwrapped_ticks(), 20);

    // 64-bit rollover
    let mut u64_unwrap = CounterUnwrapper::new(1_000_000.0, 64).unwrap();
    assert_eq!(u64_unwrap.push_raw(u64::MAX - 4), 0.0);
    assert_eq!(u64_unwrap.push_raw(5), 10_000.0);
    assert_eq!(u64_unwrap.unwrapped_ticks(), 10);
}

#[test]
fn tier1_crystal_drift_2000ppm_and_10000ppm_clamp_with_16bit_and_32bit_wrap() {
    // Part A: +2000 ppm (alpha_true = 1.002) with 16-bit wrapping counter at 1 MHz ticks
    // and realistic ±80 us host jitter over Unix-epoch base timestamp.
    let mut sync_pos = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: 1_000_000.0,
        counter_bits: 16,
    })
    .unwrap();

    let mut rng = SplitMix64(0x1234_5678_9ABC_DEF0);
    let epoch_base_ns: TimestampNs = 1_700_000_000_000_000_000;
    let mut raw_tick: u64 = 65_000;
    let mut prev_last_ns: Option<TimestampNs> = None;
    let samples_per_pkt = 1_000usize;
    let mut ticks = vec![0u64; samples_per_pkt];
    let mut out = vec![0u64; samples_per_pkt];

    for pkt_idx in 0..160u64 {
        for slot in &mut ticks {
            *slot = raw_tick & 0xFFFF;
            raw_tick = raw_tick.wrapping_add(1);
        }
        // True host time advances by 1.002 ms per 1000 device ticks (+2000 ppm)
        let ideal_host_ns = (pkt_idx as f64) * 1_000_000.0 * 1.002;
        let jitter = rng.next_signed_ns(80_000);
        let pkt_recv_ns =
            (epoch_base_ns as i128 + ideal_host_ns.round() as i128 + jitter as i128).max(0) as u64;

        sync_pos
            .assign_timestamps(pkt_recv_ns, Some(&ticks), samples_per_pkt, &mut out)
            .unwrap();

        for i in 0..samples_per_pkt {
            if i == 0 {
                if let Some(prev) = prev_last_ns {
                    assert!(out[0] >= prev, "Tier 1 regressed across packet boundary");
                }
            } else {
                assert!(out[i] >= out[i - 1], "Tier 1 regressed intra-packet");
            }
        }
        // Intra-packet spacing should reflect ~1002 ns per tick (within rounding + slope convergence)
        if pkt_idx >= 80 {
            let intra_span = (out[samples_per_pkt - 1] - out[0]) as f64;
            assert!(
                (intra_span - 999.0 * 1002.0).abs() < 1500.0,
                "unexpected intra-packet span: {intra_span}"
            );
        }
        prev_last_ns = Some(out[samples_per_pkt - 1]);
    }

    let diag_pos = sync_pos.diagnostics();
    assert!(diag_pos.locked);
    assert!(
        (diag_pos.drift_slope - 1.002).abs() < 0.0005,
        "expected slope ~1.002, got {}",
        diag_pos.drift_slope
    );

    // Part B: -2000 ppm (alpha_true = 0.998) with 32-bit wrapping counter
    let mut sync_neg = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: 1_000_000.0,
        counter_bits: 32,
    })
    .unwrap();
    raw_tick = u64::from(u32::MAX) - 10_000;
    prev_last_ns = None;
    for pkt_idx in 0..120u64 {
        for slot in &mut ticks {
            *slot = raw_tick & 0xFFFF_FFFF;
            raw_tick = raw_tick.wrapping_add(1);
        }
        let pkt_recv_ns = epoch_base_ns + ((pkt_idx as f64) * 1_000_000.0 * 0.998).round() as u64;
        sync_neg
            .assign_timestamps(pkt_recv_ns, Some(&ticks), samples_per_pkt, &mut out)
            .unwrap();
        if let Some(prev) = prev_last_ns {
            assert!(out[0] >= prev);
        }
        prev_last_ns = Some(out[samples_per_pkt - 1]);
    }
    let diag_neg = sync_neg.diagnostics();
    assert!(
        (diag_neg.drift_slope - 0.998).abs() < 0.0002,
        "expected slope ~0.998, got {}",
        diag_neg.drift_slope
    );

    // Part C: Extreme ±10000 ppm (1.010 and 0.990) MUST clamp strictly to [0.995, 1.005]
    for extreme_slope in [1.010_f64, 0.990_f64] {
        let mut tracker = LinearDriftTracker::new(64, 0.25).unwrap();
        for k in 0..100u64 {
            let t_dev = (k as f64) * 1_000_000.0;
            let t_host = 1_000_000_000 + (t_dev * extreme_slope).round() as u64;
            tracker.push_anchor(t_dev, t_host);
            let s = tracker.drift_slope();
            assert!(
                (DRIFT_SLOPE_MIN..=DRIFT_SLOPE_MAX).contains(&s),
                "slope {s} escaped [0.995, 1.005]!"
            );
        }
        let expected_clamped = extreme_slope.clamp(DRIFT_SLOPE_MIN, DRIFT_SLOPE_MAX);
        assert!(
            (tracker.drift_slope() - expected_clamped).abs() < 1e-4,
            "expected clamped slope {expected_clamped}, got {}",
            tracker.drift_slope()
        );
    }
}

#[test]
fn tier1_gap_recovery_5s_disconnect_no_retrograde() {
    let mut sync = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: 1_000_000.0,
        counter_bits: 16,
    })
    .unwrap();

    let count = 200usize;
    let mut ticks = vec![0u64; count];
    let mut out = vec![0u64; count];
    let mut last_ts = 0u64;

    for pkt in 0..20u64 {
        for (i, t) in ticks.iter_mut().enumerate() {
            *t = (pkt * (count as u64) + (i as u64)) & 0xFFFF;
        }
        let recv_ns = 1_000_000_000 + pkt * 200_000;
        sync.assign_timestamps(recv_ns, Some(&ticks), count, &mut out)
            .unwrap();
        assert!(out[0] >= last_ts);
        last_ts = out[count - 1];
    }

    // Simulate 5-second disconnect and MCU counter rebooting to 0
    for (i, t) in ticks.iter_mut().enumerate() {
        *t = i as u64;
    }
    let reconnect_recv_ns = 1_000_000_000 + 20 * 200_000 + 5_000_000_000;
    sync.assign_timestamps(reconnect_recv_ns, Some(&ticks), count, &mut out)
        .unwrap();

    assert!(out[0] >= last_ts, "backward jump after 5s disconnect");
    assert!(out[0] >= reconnect_recv_ns);
    for w in out.windows(2) {
        assert!(w[1] >= w[0]);
    }
    assert_eq!(sync.diagnostics().gap_resets, 1);
}

#[test]
fn tier2_pll_wbs_tb4_acceptance_gate_1mhz_bursty_jitter_under_1_percent() {
    // Mandatory WBS TB4 gate:
    // 1,000 samples per burst arriving every 1 ms (1 MHz stream) with ±150 us OS arrival jitter.
    let mut sync = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
        nominal_sample_rate_hz: Some(1_000_000.0),
    })
    .unwrap();

    let mut rng = SplitMix64(0xCAFE_BABE_DEAD_BEEF);
    let burst_len = 1_000usize;
    let total_packets = 400usize; // 400 ms, 400,000 samples
    let mut out = vec![0u64; burst_len];
    let mut locked_timestamps = Vec::with_capacity(250 * burst_len);
    let mut prev_last: Option<TimestampNs> = None;

    for pkt in 0..total_packets {
        let ideal_ns = 10_000_000_i64 + (pkt as i64) * 1_000_000_i64;
        let jitter_ns = rng.next_signed_ns(150_000); // ±150 us
        let recv_ns = (ideal_ns + jitter_ns).max(0) as u64;

        sync.assign_timestamps(recv_ns, None, burst_len, &mut out)
            .unwrap();

        if let Some(prev) = prev_last {
            assert!(
                out[0] > prev,
                "cross-packet strict monotonicity violated at pkt {pkt}: {prev} -> {}",
                out[0]
            );
        }
        for w in out.windows(2) {
            assert!(w[1] >= w[0], "intra-packet monotonicity violated");
        }
        prev_last = Some(out[burst_len - 1]);

        if pkt >= 100 {
            locked_timestamps.extend_from_slice(&out);
        }
    }

    let (mean_dt, std_dev, rel_std, norm_var) = interval_stats(&locked_timestamps);
    let diag = sync.diagnostics();
    println!(
        "WBS TB4 Gate (Some 1MHz): mean_dt={mean_dt:.4}ns, std_dev={std_dev:.4}ns, rel_std={:.4}%, norm_var={norm_var:.6}, rate={:.2}Hz",
        rel_std * 100.0,
        diag.estimated_rate_hz
    );
    assert!(
        (mean_dt - 1_000.0).abs() < 2.0,
        "expected mean_dt ~1000 ns, got {mean_dt}"
    );
    assert!(
        rel_std < 0.01,
        "WBS TB4 gate failed: std_dev / mean_dt = {:.4}% (must be < 1%)",
        rel_std * 100.0
    );
    assert!(
        norm_var < 0.01,
        "WBS TB4 gate failed: var / mean_dt^2 = {:.6} (must be < 0.01)",
        norm_var
    );
    assert!(diag.locked);
    assert_eq!(diag.gap_resets, 0);
    assert!((diag.estimated_rate_hz - 1_000_000.0).abs() < 5_000.0);
}

#[test]
fn tier2_pll_adaptive_rate_estimation_none_over_50ms_window() {
    // Unknown nominal sample rate (`None`), actual stream = 1 MHz (1,000 pts per 1 ms) + ±150 us jitter.
    let mut sync = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
        nominal_sample_rate_hz: None,
    })
    .unwrap();

    let mut rng = SplitMix64(0x0BAD_F00D_1337_4242);
    let burst_len = 1_000usize;
    let mut out = vec![0u64; burst_len];
    let mut all_prelock_last: Option<TimestampNs> = None;
    let mut post_lock_timestamps = Vec::with_capacity(200 * burst_len);

    for pkt in 0..300usize {
        let ideal_ns = 5_000_000_i64 + (pkt as i64) * 1_000_000_i64;
        let jitter_ns = rng.next_signed_ns(150_000);
        let recv_ns = (ideal_ns + jitter_ns).max(0) as u64;

        sync.assign_timestamps(recv_ns, None, burst_len, &mut out)
            .unwrap();

        if let Some(prev) = all_prelock_last {
            assert!(
                out[0] > prev,
                "monotonicity violated during adaptive startup at pkt {pkt}: {prev} -> {}",
                out[0]
            );
        }
        for w in out.windows(2) {
            assert!(w[1] >= w[0]);
        }
        all_prelock_last = Some(out[burst_len - 1]);

        if pkt < 45 {
            assert!(
                !sync.diagnostics().locked,
                "should still be in 50ms adaptive window at pkt {pkt}"
            );
        }
        if pkt >= 60 {
            assert!(
                sync.diagnostics().locked,
                "should be locked after 50ms adaptive window at pkt {pkt}"
            );
            post_lock_timestamps.extend_from_slice(&out);
        }
    }

    let diag = sync.diagnostics();
    assert!(
        (diag.estimated_rate_hz - 1_000_000.0).abs() < 10_000.0,
        "adaptive estimated rate {} too far from 1 MHz",
        diag.estimated_rate_hz
    );
    let (mean_dt, std_dev, rel_std, norm_var) = interval_stats(&post_lock_timestamps);
    println!(
        "WBS TB4 Adaptive (None->1MHz): mean_dt={mean_dt:.4}ns, std_dev={std_dev:.4}ns, rel_std={:.4}%, norm_var={norm_var:.6}, rate={:.2}Hz",
        rel_std * 100.0,
        diag.estimated_rate_hz
    );
    assert!(
        (mean_dt - 1_000.0).abs() < 5.0,
        "unexpected adaptive mean_dt: {mean_dt}"
    );
    assert!(
        rel_std < 0.01 && norm_var < 0.01,
        "adaptive PLL post-lock variance exceeded 1%: rel_std={rel_std}, norm_var={norm_var}"
    );
}

#[test]
fn tier2_pll_tracks_clock_offset_and_recovers_from_5s_pause() {
    let mut sync = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
        nominal_sample_rate_hz: Some(1_000_000.0),
    })
    .unwrap();

    let burst_len = 1_000usize;
    let mut out = vec![0u64; burst_len];
    let mut prev_last = 0u64;
    let mut t_recv = 1_000_000_u64;

    // Source runs +2000 ppm slower per sample (dt_true = 1002 ns, rate ~998_004 Hz)
    for pkt in 0..3_000u64 {
        if pkt == 2_500 {
            // 5-second pause / disconnect
            t_recv += 5_000_000_000;
        }
        sync.assign_timestamps(t_recv, None, burst_len, &mut out)
            .unwrap();
        if pkt > 0 {
            assert!(out[0] > prev_last, "backward jump at pkt {pkt}");
        }
        for w in out.windows(2) {
            assert!(w[1] >= w[0]);
        }
        prev_last = out[burst_len - 1];
        t_recv += 1_002_000;
    }

    let diag = sync.diagnostics();
    assert_eq!(diag.gap_resets, 1);
    assert!(
        (diag.estimated_dt_ns - 1_002.0).abs() < 0.2,
        "PLL failed to preserve/track dt_hat ~1002 ns after pause, got {}",
        diag.estimated_dt_ns
    );
}

#[test]
fn tier3_host_arrival_is_monotonic_across_nonmonotonic_inputs() {
    let mut sync = StreamClockSynchronizer::new(ClockMode::Tier3HostArrival).unwrap();
    let mut out = [0u64; 4];

    sync.assign_timestamps(1_000_000, None, 4, &mut out).unwrap();
    assert_eq!(out, [1_000_000; 4]);

    // Simulate minor OS clock jitter / non-monotonic caller value
    sync.assign_timestamps(999_500, None, 3, &mut out).unwrap();
    assert_eq!(&out[..3], &[1_000_000; 3]);

    sync.assign_timestamps(1_250_000, None, 2, &mut out).unwrap();
    assert_eq!(&out[..2], &[1_250_000; 2]);
    assert!(sync.diagnostics().locked);
}

#[test]
fn decoded_batch_population_zero_realloc_and_error_handling() {
    let descs = [ChannelDescriptor::new_scalar("sim0", "ia", ScalarType::F32)];
    let row_cap = 1_024usize;
    let mut batch = DecodedBatch::from_descriptors(&descs, row_cap);

    let mut sync = StreamClockSynchronizer::new(ClockMode::Tier2SoftwarePll {
        nominal_sample_rate_hz: Some(1_000_000.0),
    })
    .unwrap();

    // Warm up batch timestamps allocation
    batch.sample_rows = 512;
    batch.packet_recv_ns = 1_000_000;
    sync.populate_batch_timestamps(&mut batch).unwrap();
    assert_eq!(batch.timestamps_ns.len(), 512);
    let warm_ptr = batch.timestamps_ns.as_ptr();
    let warm_cap = batch.timestamps_ns.capacity();
    assert!(warm_cap >= row_cap);

    // Subsequent batches up to `row_cap` must reuse the exact same heap buffer
    for k in 1..20u64 {
        batch.clear();
        batch.sample_rows = 1_000;
        batch.packet_recv_ns = 1_000_000 + k * 1_000_000;
        sync.populate_batch_timestamps(&mut batch).unwrap();
        assert_eq!(batch.timestamps_ns.len(), 1_000);
        assert_eq!(batch.timestamps_ns.as_ptr(), warm_ptr);
        assert_eq!(batch.timestamps_ns.capacity(), warm_cap);
    }

    // Empty batch is a clean no-op
    batch.clear();
    batch.sample_rows = 0;
    sync.populate_batch_timestamps(&mut batch).unwrap();
    assert!(batch.timestamps_ns.is_empty());

    // Tier 1 requires hw_ticks matching sample_rows
    let mut tier1_sync = StreamClockSynchronizer::new(ClockMode::Tier1HardwareCounter {
        tick_hz: 1_000_000.0,
        counter_bits: 16,
    })
    .unwrap();
    batch.clear();
    batch.sample_rows = 10;
    batch.packet_recv_ns = 5_000_000;
    let err = tier1_sync.populate_batch_timestamps(&mut batch).unwrap_err();
    assert!(matches!(err, CoreError::Clock(_)));
    assert!(batch.timestamps_ns.is_empty());

    // Providing matching hw_ticks populates batch cleanly
    batch.hw_ticks.extend(0..10);
    tier1_sync.populate_batch_timestamps(&mut batch).unwrap();
    assert_eq!(batch.timestamps_ns.len(), 10);
    assert_eq!(batch.timestamps_ns[0], 5_000_000);
    assert_eq!(batch.timestamps_ns[9], 5_009_000);

    // Reset clears diagnostics back to unlocked
    tier1_sync.reset();
    let diag = tier1_sync.diagnostics();
    assert!(!diag.locked);
    assert_eq!(diag.gap_resets, 0);
    assert_eq!(diag.drift_slope, 1.0);
}
