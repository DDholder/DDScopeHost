use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ddscope_core::ddsf::{
    DdsfBlockKind, DdsfFramePacker, DdsfZeroCopyReader, ScopePacketView, TrackBlockView,
    TrackPayloadSlice,
};
use ddscope_core::CoreWorkspace;
use ddscope_desktop_lib::commands::{
    ClockConfigDto, CreateSourceRequestDto, DesktopBridgeState, ProtocolConfigDto,
    ScopeLaneKindDto, ScopeTrackBindingDto, ScopeViewportConfigDto, TransportConfigDto,
    TriggerSweepModeDto,
};
use ddscope_desktop_lib::data_bridge::{
    DataBridgeModeDto, DdsfSlotReadyDto, FrameSink, RawIpcFrameSink, ZeroCopyDataBridge,
    DDSF_SHM_SLOT_BYTES, DDSF_SHM_SLOT_COUNT,
};
use tauri::ipc::{Channel, InvokeResponseBody};

const TARGET_2MIB_FRAME_BYTES: usize = 2 * 1024 * 1024;
const OVERSIZED_FRAME_BYTES: usize = DDSF_SHM_SLOT_BYTES + 64 * 1024;

fn build_ddsf_spectrum_frame(frame_seq: u32, total_target_bytes: usize) -> Vec<u8> {
    const OVERHEAD: usize = 64 + 32 + 32;
    assert!(total_target_bytes >= OVERHEAD);
    let f32_len = (total_target_bytes - OVERHEAD) / std::mem::size_of::<f32>();
    let mut bins = vec![0.25_f32; f32_len];
    if let Some(first) = bins.first_mut() {
        *first = frame_seq as f32;
    }

    let track = TrackBlockView {
        channel_id: 101,
        block_kind: DdsfBlockKind::SpectrumFrame,
        digital_init_level: 0,
        flags: 0,
        stats: None,
        payload: TrackPayloadSlice::SpectrumFrame(&bins),
        aux_f32_0: 0.0,
        aux_f32_1: 1_000.0,
    };
    let tracks = [track];
    let scope = ScopePacketView {
        scope_id: 1,
        trigger_state: 0,
        mode: 0,
        win_t_start_ns: 0,
        win_t_span_ns: 20_000_000,
        subpixel_offset_ns: 0.0,
        tracks: &tracks,
    };

    let mut packer = DdsfFramePacker::with_capacity(total_target_bytes);
    packer
        .pack_frame_borrowed(
            frame_seq,
            u64::from(frame_seq) * 8_333_333,
            0,
            &[scope],
            &[],
        )
        .expect("pack DDSF v1 frame")
        .to_vec()
}

#[cfg(windows)]
fn process_cpu_time_100ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;

    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    assert_ne!(ok, 0, "GetProcessTimes failed");

    let ft_u64 = |ft: FILETIME| (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    ft_u64(kernel).saturating_add(ft_u64(user))
}

#[cfg(windows)]
#[test]
fn shm_ring_triple_slot_zero_copy_and_oversized_fallback() {
    use ddscope_desktop_lib::data_bridge::shm_windows::{
        ShmRingBuffer, WebView2SharedBufferSink,
    };

    let ring = ShmRingBuffer::new_anonymous_for_test().expect("allocate 3 x 8 MiB OS SHM slots");
    let ro_views = Arc::new(
        ring.open_readonly_views_for_test()
            .expect("open read-only OS mappings for 3 slots"),
    );

    let ro_for_cb = Arc::clone(&ro_views);
    let verified_seqs = Arc::new(parking_lot::Mutex::new(Vec::<(u8, u32, usize)>::new()));
    let verified_for_cb = Arc::clone(&verified_seqs);

    let slot_notify_channel = Channel::new(move |body| {
        if let InvokeResponseBody::Json(json) = body {
            let msg: DdsfSlotReadyDto =
                serde_json::from_str(&json).expect("deserialize DdsfSlotReadyDto");
            let slot_idx = usize::from(msg.slot);
            assert!(slot_idx < DDSF_SHM_SLOT_COUNT);
            let slice = &ro_for_cb[slot_idx].as_slice()[..msg.byte_len as usize];
            let reader = DdsfZeroCopyReader::new(slice).expect("valid DDSF v1 in OS SHM view");
            assert_eq!(reader.header().frame_seq, msg.frame_seq);
            verified_for_cb
                .lock()
                .push((msg.slot, msg.frame_seq, msg.byte_len as usize));
        } else {
            panic!("expected InvokeResponseBody::Json on slot_notify_channel");
        }
        Ok(())
    });

    let raw_frames = Arc::new(parking_lot::Mutex::new(Vec::<(u32, usize)>::new()));
    let raw_for_cb = Arc::clone(&raw_frames);
    let oversized_raw_channel = Channel::new(move |body| {
        if let InvokeResponseBody::Raw(bytes) = body {
            let reader = DdsfZeroCopyReader::new(&bytes).expect("valid oversized raw DDSF v1");
            raw_for_cb
                .lock()
                .push((reader.header().frame_seq, bytes.len()));
        } else {
            panic!("expected InvokeResponseBody::Raw on oversized_raw_channel");
        }
        Ok(())
    });

    let sink: Arc<dyn FrameSink> = Arc::new(WebView2SharedBufferSink::new(
        ring,
        slot_notify_channel,
        oversized_raw_channel,
    ));
    let bridge = ZeroCopyDataBridge::new();
    bridge.attach_sink(sink);

    for seq in 1_u32..=6 {
        let frame = build_ddsf_spectrum_frame(seq, TARGET_2MIB_FRAME_BYTES);
        assert_eq!(frame.len(), TARGET_2MIB_FRAME_BYTES);
        let actual = bridge.push_frame(&frame).expect("push 2 MiB SHM frame");
        assert_eq!(actual, DataBridgeModeDto::WebView2SharedBuffer);
    }

    let recorded = verified_seqs.lock().clone();
    assert_eq!(recorded.len(), 6);
    assert_eq!(
        recorded,
        vec![
            (1, 1, TARGET_2MIB_FRAME_BYTES),
            (2, 2, TARGET_2MIB_FRAME_BYTES),
            (0, 3, TARGET_2MIB_FRAME_BYTES),
            (1, 4, TARGET_2MIB_FRAME_BYTES),
            (2, 5, TARGET_2MIB_FRAME_BYTES),
            (0, 6, TARGET_2MIB_FRAME_BYTES),
        ]
    );

    // Push > 8 MiB frame and verify automatic per-frame fallback to RawIpcChannel.
    let oversized = build_ddsf_spectrum_frame(7, OVERSIZED_FRAME_BYTES);
    assert!(oversized.len() > DDSF_SHM_SLOT_BYTES);
    let fallback_mode = bridge
        .push_frame(&oversized)
        .expect("oversized frame should transparently fall back to RawIpcChannel");
    assert_eq!(fallback_mode, DataBridgeModeDto::RawIpcChannel);

    let raw_recorded = raw_frames.lock().clone();
    assert_eq!(raw_recorded, vec![(7, oversized.len())]);

    let stats = bridge.stats_dto();
    assert_eq!(stats.active_mode, Some(DataBridgeModeDto::WebView2SharedBuffer));
    assert_eq!(stats.shm_frames_pushed, 6.0);
    assert_eq!(stats.raw_ipc_frames_pushed, 1.0);
    assert_eq!(stats.oversized_fallbacks, 1);
    assert_eq!(stats.send_errors, 0);
    assert_eq!(stats.last_frame_seq, 7);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_workspace_step_and_push_ddsf_frame_zero_copy() {
    let ws = Arc::new(CoreWorkspace::new(tokio::runtime::Handle::current()));
    let state = DesktopBridgeState::with_workspace(Arc::clone(&ws));

    state
        .create_source(CreateSourceRequestDto {
            source_id: "sim0".to_string(),
            display_name: Some("Sim DataPlane".to_string()),
            transport: TransportConfigDto::Sim {
                sample_rate_hz: 2_000.0,
                analog_channels: 2,
                enable_glitch: false,
                glitch_period_samples: 0,
            },
            clock: ClockConfigDto::Tier3HostArrival,
            protocol: ProtocolConfigDto::SimPreset,
            auto_start: true,
        })
        .expect("create & auto-start sim0");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let tree = state.get_channel_tree().expect("channel tree");
    let ch_id = tree.sources[0].channels[0].channel_id;

    state
        .upsert_scope_viewport(ScopeViewportConfigDto {
            scope_id: 1,
            title: Some("Bridge Live".to_string()),
            canvas_width_px: 800,
            time_span_ns: 20_000_000.0,
            pre_trigger_ratio: 0.2,
            pan_offset_ns: 0.0,
            sweep_mode: TriggerSweepModeDto::Roll,
            auto_timeout_ms: 10,
            trigger: None,
            tracks: vec![ScopeTrackBindingDto {
                channel_id: ch_id,
                lane: ScopeLaneKindDto::Analog,
                visible: true,
                y_scale: Some(1.0),
                y_offset: Some(0.0),
                color_rgba: Some(0x38_BD_F8_FF),
            }],
            sync_group_id: None,
            paused: false,
        })
        .expect("upsert scope 1");

    let received_raw = Arc::new(AtomicUsize::new(0));
    let raw_cnt = Arc::clone(&received_raw);
    let raw_ch = Channel::new(move |body| {
        if let InvokeResponseBody::Raw(bytes) = body {
            let reader = DdsfZeroCopyReader::new(&bytes).expect("valid live raw DDSF frame");
            assert!(reader.header().scope_count >= 1);
            raw_cnt.fetch_add(1, Ordering::Relaxed);
        } else {
            panic!("expected Raw IPC response body");
        }
        Ok(())
    });

    state
        .data_bridge()
        .attach_sink(Arc::new(RawIpcFrameSink::new(raw_ch)));

    for step in 1_u64..=3 {
        let mode = state
            .step_and_push_ddsf_frame(step * 10_000_000)
            .expect("step_and_push_ddsf_frame")
            .expect("attached live bridge emits mode");
        assert_eq!(mode, DataBridgeModeDto::RawIpcChannel);
    }
    assert_eq!(received_raw.load(Ordering::Relaxed), 3);

    #[cfg(windows)]
    {
        use ddscope_desktop_lib::data_bridge::shm_windows::{
            ShmRingBuffer, WebView2SharedBufferSink,
        };

        let ring = ShmRingBuffer::new_anonymous_for_test().expect("shm ring");
        let ro_views = Arc::new(ring.open_readonly_views_for_test().expect("ro views"));
        let ro_cb = Arc::clone(&ro_views);
        let received_shm = Arc::new(AtomicUsize::new(0));
        let shm_cnt = Arc::clone(&received_shm);

        let slot_ch = Channel::new(move |body| {
            if let InvokeResponseBody::Json(json) = body {
                let msg: DdsfSlotReadyDto = serde_json::from_str(&json).expect("slot dto");
                let slice = &ro_cb[usize::from(msg.slot)].as_slice()[..msg.byte_len as usize];
                let reader = DdsfZeroCopyReader::new(slice).expect("valid live SHM DDSF frame");
                assert_eq!(reader.header().frame_seq, msg.frame_seq);
                shm_cnt.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        });
        let unused_raw = Channel::new(|_| Ok(()));

        state
            .data_bridge()
            .attach_sink(Arc::new(WebView2SharedBufferSink::new(
                ring,
                slot_ch,
                unused_raw,
            )));

        for step in 4_u64..=6 {
            let mode = state
                .step_and_push_ddsf_frame(step * 10_000_000)
                .expect("step_and_push_ddsf_frame SHM")
                .expect("attached live SHM bridge emits mode");
            assert_eq!(mode, DataBridgeModeDto::WebView2SharedBuffer);
        }
        assert_eq!(received_shm.load(Ordering::Relaxed), 3);
    }

    state.detach_data_bridge().expect("detach_data_bridge");
    state
        .remove_scope_viewport(1)
        .expect("remove_scope_viewport");
    state
        .stop_source("sim0".to_string())
        .await
        .expect("stop_source");
    state
        .remove_source("sim0".to_string())
        .await
        .expect("remove_source");
    ws.shutdown().await.expect("shutdown workspace");
}

#[cfg(windows)]
#[test]
fn bench_sustained_120hz_2mib_shm_and_raw_ipc_cpu_gate() {
    use ddscope_desktop_lib::data_bridge::shm_windows::{
        ShmRingBuffer, WebView2SharedBufferSink,
    };

    const FRAME_COUNT: usize = 120;
    const TARGET_FPS: f64 = 120.0;
    let period = Duration::from_secs_f64(1.0 / TARGET_FPS);
    let logical_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f64;

    // Pre-build 3 valid 2.000 MiB DDSF v1 frames matching `(frame_seq % 3)` so per-frame
    // benchmark measures pure zero-copy bridge & IPC cost rather than test-fixture allocation.
    let prebuilt_frames: Vec<Vec<u8>> = (0..DDSF_SHM_SLOT_COUNT)
        .map(|idx| {
            let frame = build_ddsf_spectrum_frame(idx as u32, TARGET_2MIB_FRAME_BYTES);
            assert_eq!(frame.len(), TARGET_2MIB_FRAME_BYTES);
            frame
        })
        .collect();

    // -------------------------------------------------------------------------
    // Mode A: WebView2SharedBufferSink (OS 3 x 8 MiB SHM + JSON slot notify)
    // -------------------------------------------------------------------------
    let ring = ShmRingBuffer::new_anonymous_for_test().expect("allocate 3 x 8 MiB SHM slots");
    let ro_views = Arc::new(
        ring.open_readonly_views_for_test()
            .expect("open read-only OS mappings"),
    );
    let ro_cb = Arc::clone(&ro_views);
    let shm_parsed_frames = Arc::new(AtomicUsize::new(0));
    let shm_parsed_bytes = Arc::new(AtomicU64::new(0));
    let shm_parsed_frames_cb = Arc::clone(&shm_parsed_frames);
    let shm_parsed_bytes_cb = Arc::clone(&shm_parsed_bytes);

    let slot_ch = Channel::new(move |body| {
        if let InvokeResponseBody::Json(json) = body {
            let msg: DdsfSlotReadyDto = serde_json::from_str(&json).expect("slot ready JSON");
            let slot_idx = usize::from(msg.slot);
            let slice = &ro_cb[slot_idx].as_slice()[..msg.byte_len as usize];
            let reader = DdsfZeroCopyReader::new(slice).expect("valid SHM DDSF v1");
            let hdr = reader.header();
            shm_parsed_bytes_cb.fetch_add(hdr.total_byte_len, Ordering::Relaxed);
            shm_parsed_frames_cb.fetch_add(1, Ordering::Relaxed);
        } else {
            panic!("expected InvokeResponseBody::Json");
        }
        Ok(())
    });
    let oversized_fallback_ch = Channel::new(|_| Ok(()));

    let bridge_shm = ZeroCopyDataBridge::new();
    bridge_shm.attach_sink(Arc::new(WebView2SharedBufferSink::new(
        ring,
        slot_ch,
        oversized_fallback_ch,
    )));

    // Warm up one cycle so demand-zero page faults settle before timing.
    for frame in prebuilt_frames.iter().take(DDSF_SHM_SLOT_COUNT) {
        bridge_shm.push_frame(frame).expect("SHM warmup");
    }
    shm_parsed_frames.store(0, Ordering::Relaxed);
    shm_parsed_bytes.store(0, Ordering::Relaxed);

    let wall_start_shm = Instant::now();
    let cpu_start_shm = process_cpu_time_100ns();
    let mut next_deadline = wall_start_shm + period;

    for i in 0..FRAME_COUNT {
        let frame = &prebuilt_frames[i % DDSF_SHM_SLOT_COUNT];
        bridge_shm.push_frame(frame).expect("SHM push 2 MiB frame");

        if let Some(rem) = next_deadline.checked_duration_since(Instant::now()) {
            if rem >= Duration::from_millis(2) {
                std::thread::sleep(rem);
            }
        }
        next_deadline += period;
    }

    let cpu_delta_shm_100ns = process_cpu_time_100ns().saturating_sub(cpu_start_shm);
    let wall_shm_secs = wall_start_shm.elapsed().as_secs_f64();
    let cpu_shm_secs = (cpu_delta_shm_100ns as f64) / 10_000_000.0;
    let shm_single_core_pct = (cpu_shm_secs / wall_shm_secs) * 100.0;
    let shm_system_cpu_pct = shm_single_core_pct / logical_cpus;
    let total_shm_bytes = shm_parsed_bytes.load(Ordering::Relaxed);
    let shm_mib_per_sec = ((total_shm_bytes as f64) / (1024.0 * 1024.0)) / wall_shm_secs;

    println!(
        "[TD2 Bench] WebView2SharedBuffer: frames={}, total_mib={:.2}, wall={:.4}s, throughput={:.2} MiB/s, system_cpu={:.3}%, single_core_cpu={:.3}%, logical_cpus={:.0}",
        shm_parsed_frames.load(Ordering::Relaxed),
        (total_shm_bytes as f64) / (1024.0 * 1024.0),
        wall_shm_secs,
        shm_mib_per_sec,
        shm_system_cpu_pct,
        shm_single_core_pct,
        logical_cpus,
    );

    assert_eq!(shm_parsed_frames.load(Ordering::Relaxed), FRAME_COUNT);
    assert_eq!(
        total_shm_bytes,
        (FRAME_COUNT * TARGET_2MIB_FRAME_BYTES) as u64
    );
    assert!(
        shm_system_cpu_pct < 2.0,
        "WebView2SharedBuffer IPC system CPU exceeded 2.0% gate: {shm_system_cpu_pct:.3}%"
    );

    // -------------------------------------------------------------------------
    // Mode B: RawIpcFrameSink (InvokeResponseBody::Raw Vec<u8>)
    // -------------------------------------------------------------------------
    let raw_parsed_frames = Arc::new(AtomicUsize::new(0));
    let raw_parsed_bytes = Arc::new(AtomicU64::new(0));
    let raw_parsed_frames_cb = Arc::clone(&raw_parsed_frames);
    let raw_parsed_bytes_cb = Arc::clone(&raw_parsed_bytes);

    let raw_ch = Channel::new(move |body| {
        if let InvokeResponseBody::Raw(bytes) = body {
            let reader = DdsfZeroCopyReader::new(&bytes).expect("valid raw IPC DDSF v1");
            raw_parsed_bytes_cb.fetch_add(reader.header().total_byte_len, Ordering::Relaxed);
            raw_parsed_frames_cb.fetch_add(1, Ordering::Relaxed);
        } else {
            panic!("expected InvokeResponseBody::Raw");
        }
        Ok(())
    });

    let bridge_raw = ZeroCopyDataBridge::new();
    bridge_raw.attach_sink(Arc::new(RawIpcFrameSink::new(raw_ch)));
    bridge_raw
        .push_frame(&prebuilt_frames[0])
        .expect("Raw IPC warmup");
    raw_parsed_frames.store(0, Ordering::Relaxed);
    raw_parsed_bytes.store(0, Ordering::Relaxed);

    let wall_start_raw = Instant::now();
    let cpu_start_raw = process_cpu_time_100ns();
    let mut next_deadline_raw = wall_start_raw + period;

    for i in 0..FRAME_COUNT {
        let frame = &prebuilt_frames[i % DDSF_SHM_SLOT_COUNT];
        bridge_raw
            .push_frame(frame)
            .expect("Raw IPC push 2 MiB frame");

        if let Some(rem) = next_deadline_raw.checked_duration_since(Instant::now()) {
            if rem >= Duration::from_millis(2) {
                std::thread::sleep(rem);
            }
        }
        next_deadline_raw += period;
    }

    let cpu_delta_raw_100ns = process_cpu_time_100ns().saturating_sub(cpu_start_raw);
    let wall_raw_secs = wall_start_raw.elapsed().as_secs_f64();
    let cpu_raw_secs = (cpu_delta_raw_100ns as f64) / 10_000_000.0;
    let raw_single_core_pct = (cpu_raw_secs / wall_raw_secs) * 100.0;
    let raw_system_cpu_pct = raw_single_core_pct / logical_cpus;
    let total_raw_bytes = raw_parsed_bytes.load(Ordering::Relaxed);
    let raw_mib_per_sec = ((total_raw_bytes as f64) / (1024.0 * 1024.0)) / wall_raw_secs;

    println!(
        "[TD2 Bench] RawIpcChannel:      frames={}, total_mib={:.2}, wall={:.4}s, throughput={:.2} MiB/s, system_cpu={:.3}%, single_core_cpu={:.3}%, logical_cpus={:.0}",
        raw_parsed_frames.load(Ordering::Relaxed),
        (total_raw_bytes as f64) / (1024.0 * 1024.0),
        wall_raw_secs,
        raw_mib_per_sec,
        raw_system_cpu_pct,
        raw_single_core_pct,
        logical_cpus,
    );

    assert_eq!(raw_parsed_frames.load(Ordering::Relaxed), FRAME_COUNT);
    assert_eq!(
        total_raw_bytes,
        (FRAME_COUNT * TARGET_2MIB_FRAME_BYTES) as u64
    );
    assert!(
        raw_system_cpu_pct < 2.0,
        "RawIpcChannel IPC system CPU exceeded 2.0% gate: {raw_system_cpu_pct:.3}%"
    );
}
