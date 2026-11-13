//! End-to-end and throughput tests for Milestone TB5 (Protocol Engine MVP).

use std::time::Instant;

use bytes::BytesMut;
use ddscope_core::protocol::{
    compute_checksum, encode_hex_string, encode_sim_preset_frame, sim_preset_flags,
    BasicDownlinkEncoder, ChecksumKind, ChecksumScope, DownlinkPayload, FireWaterColumnSpec,
    FireWaterConfig, FireWaterCsvCodec, FixedFieldSpec, FixedSchemaBinaryCodec,
    FixedSchemaBinaryConfig, JustFloatCodec, JustFloatConfig, NonFinitePolicy, ProtocolCodec,
    ProtocolRegistry, SimPresetCodec, SimPresetConfig, JUSTFLOAT_TAIL_MAGIC, SIM_PRESET_MAGIC,
    SIM_PRESET_TAIL,
};
use ddscope_core::types::{
    BitfieldSpec, ChannelKind, DecodedBatch, DecodedColumnBuf, Endianness, EventSchemaKind,
    LinearScale, ScalarType, ScalarValue, StateEnumMap, StateLabel,
};

fn encode_justfloat_frame(values: &[f32], out: &mut Vec<u8>) {
    for &v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&JUSTFLOAT_TAIL_MAGIC);
}

#[test]
fn justfloat_one_byte_streaming_and_10000_frame_sticky_buffer() {
    let cfg = JustFloatConfig::new(
        "udp0",
        vec!["ia".into(), "ib".into(), "ic".into(), "vbus".into()],
    );
    let mut codec = JustFloatCodec::new(cfg).unwrap();
    let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 10_000);

    const FRAMES: usize = 10_000;
    let mut sticky = Vec::with_capacity(FRAMES * 20);
    for i in 0..FRAMES {
        let base = i as f32 * 0.25;
        encode_justfloat_frame(&[base, base + 1.0, -base, 24.0 + (i % 7) as f32], &mut sticky);
    }

    // 10,000-frame sticky buffer in one feed_bytes call.
    let rows = codec.feed_bytes(&sticky, 100_000, &mut batch).unwrap();
    assert_eq!(rows, FRAMES);
    assert_eq!(batch.sample_rows, FRAMES);
    assert_eq!(batch.packet_recv_ns, 100_000);
    assert_eq!(
        batch.columns[0].as_scalar_slice().unwrap().get_as_f64(9999),
        Some((9999.0_f32 * 0.25) as f64)
    );
    assert_eq!(
        batch.columns[3].as_scalar_slice().unwrap().get_as_f64(7),
        Some(24.0)
    );

    // Capacity retention across clear().
    let cap_before = match &batch.columns[0] {
        DecodedColumnBuf::F32(v) => v.capacity(),
        _ => unreachable!(),
    };
    batch.clear();
    assert!(batch.is_empty());
    let cap_after = match &batch.columns[0] {
        DecodedColumnBuf::F32(v) => v.capacity(),
        _ => unreachable!(),
    };
    assert_eq!(cap_before, cap_after);

    // 1-byte-at-a-time partial streaming on 100 frames.
    codec.reset_state();
    let sample_slice = &sticky[..100 * 20];
    let mut total_rows = 0;
    for (idx, &byte) in sample_slice.iter().enumerate() {
        total_rows += codec
            .feed_bytes(&[byte], 200_000 + idx as u64, &mut batch)
            .unwrap();
    }
    assert_eq!(total_rows, 100);
    assert_eq!(batch.sample_rows, 100);
    for i in 0..100 {
        assert_eq!(
            batch.columns[0].as_scalar_slice().unwrap().get_as_f64(i),
            Some((i as f32 * 0.25) as f64)
        );
    }
}

#[test]
fn justfloat_garbage_resync_and_nan_inf_rejection() {
    let cfg = JustFloatConfig::new("udp0", vec!["ch0".into(), "ch1".into(), "ch2".into()]);
    let mut codec = JustFloatCodec::new(cfg).unwrap();
    let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 64);

    let mut rng = fastrand::Rng::with_seed(0xDEAD_BEEF_CAFE_BABEu64);
    let mut wire = Vec::new();
    let mut expected = Vec::new();

    for step in 0..30u32 {
        // Inject random garbage bytes (avoiding accidental full magic sequence).
        let garbage_len = rng.usize(1..=19);
        for _ in 0..garbage_len {
            let b = rng.u8(..);
            if b != 0x7F {
                wire.push(b);
            } else {
                wire.push(0x7E);
            }
        }
        // Corrupted frame (prepended garbage makes length != 12).
        encode_justfloat_frame(&[1.0, 2.0, 3.0], &mut wire);

        // Non-finite frames must be rejected cleanly.
        encode_justfloat_frame(&[f32::NAN, 1.0, 2.0], &mut wire);
        encode_justfloat_frame(&[1.0, f32::INFINITY, 2.0], &mut wire);
        encode_justfloat_frame(&[1.0, 2.0, f32::NEG_INFINITY], &mut wire);

        // Valid frame immediately following must decode intact.
        let vals = [step as f32 * 1.5, -3.25, 24.125];
        encode_justfloat_frame(&vals, &mut wire);
        expected.push(vals);
    }

    // Feed in random small chunks (1..=17 bytes).
    let mut cursor = 0usize;
    while cursor < wire.len() {
        let chunk_len = rng.usize(1..=17).min(wire.len() - cursor);
        codec
            .feed_bytes(&wire[cursor..cursor + chunk_len], 50_000, &mut batch)
            .unwrap();
        cursor += chunk_len;
    }

    assert_eq!(batch.sample_rows, expected.len());
    // Note: f32::INFINITY (0x7F800000) is byte-identical to JUSTFLOAT_TAIL_MAGIC, so when a chunk
    // boundary splits inside a +inf frame it is counted as length_mismatch rather than non_finite.
    assert!(codec.stats().length_mismatch_frames >= 30);
    assert!(codec.stats().non_finite_frames >= 60);
    for (idx, exp) in expected.iter().enumerate() {
        for (col, &ev) in exp.iter().enumerate() {
            assert_eq!(
                batch.columns[col].as_scalar_slice().unwrap().get_as_f64(idx),
                Some(f64::from(ev))
            );
        }
    }

    // Verify ReplaceWithZero policy.
    let mut cfg_zero = JustFloatConfig::new("udp0", vec!["a".into(), "b".into()]);
    cfg_zero.non_finite_policy = NonFinitePolicy::ReplaceWithZero;
    let mut codec_zero = JustFloatCodec::new(cfg_zero).unwrap();
    let mut batch_zero = DecodedBatch::from_descriptors(codec_zero.declared_channels(), 4);
    let mut z_wire = Vec::new();
    encode_justfloat_frame(&[f32::NAN, 42.5], &mut z_wire);
    assert_eq!(codec_zero.feed_bytes(&z_wire, 1, &mut batch_zero).unwrap(), 1);
    assert_eq!(
        batch_zero.columns[0].as_scalar_slice().unwrap().get_as_f64(0),
        Some(0.0)
    );
    assert_eq!(
        batch_zero.columns[1].as_scalar_slice().unwrap().get_as_f64(0),
        Some(42.5)
    );
}

#[test]
fn fixed_schema_binary_all_scalar_types_endian_scale_bitfields_and_checksums() {
    let mut state_labels = StateEnumMap::new();
    state_labels.insert(
        0,
        StateLabel {
            name: "IDLE".into(),
            color_rgba: None,
        },
    );
    state_labels.insert(
        5,
        StateLabel {
            name: "ACTIVE".into(),
            color_rgba: None,
        },
    );

    let fields = vec![
        FixedFieldSpec::scalar("hw_tick", ScalarType::U32), // offset 0..4 -> hw_ticks
        FixedFieldSpec::scalar("b_flag", ScalarType::Bool), // offset 4..5
        FixedFieldSpec::scalar("u8_v", ScalarType::U8),     // offset 5..6
        FixedFieldSpec::scalar("u16_be", ScalarType::U16).with_endian(Endianness::Big), // 6..8
        FixedFieldSpec::scalar("u32_be", ScalarType::U32).with_endian(Endianness::Big), // 8..12
        FixedFieldSpec::scalar("u64_le", ScalarType::U64),  // 12..20
        FixedFieldSpec::scalar("i8_v", ScalarType::I8),     // 20..21
        FixedFieldSpec::scalar("i16_le", ScalarType::I16)
            .with_scale(LinearScale::new(0.01, -1.0))
            .with_unit("A"), // 21..23
        FixedFieldSpec::scalar("i32_be", ScalarType::I32).with_endian(Endianness::Big), // 23..27
        FixedFieldSpec::scalar("i64_be", ScalarType::I64).with_endian(Endianness::Big), // 27..35
        FixedFieldSpec::scalar("f32_be", ScalarType::F32).with_endian(Endianness::Big), // 35..39
        FixedFieldSpec::scalar("f64_le", ScalarType::F64),  // 39..47
        FixedFieldSpec::bitfield(
            "bf_bool",
            ScalarType::Bool,
            BitfieldSpec {
                bit_offset: 3,
                bit_width: 1,
            },
            Some(47),
        ),
        FixedFieldSpec::bitfield(
            "bf_signed",
            ScalarType::I8,
            BitfieldSpec {
                bit_offset: 4,
                bit_width: 4,
            },
            Some(47),
        )
        .with_carrier_type(ScalarType::U8),
        FixedFieldSpec::state_enum("fsm", ScalarType::U8, state_labels.clone()).with_offset(48),
    ];

    for checksum in [
        ChecksumKind::None,
        ChecksumKind::Sum8,
        ChecksumKind::Xor8,
        ChecksumKind::Crc16Ccitt,
        ChecksumKind::Crc32,
    ] {
        let cfg = FixedSchemaBinaryConfig {
            source_id: "mcu".into(),
            header_magic: vec![0xA5, 0x5A],
            tail_magic: Some(vec![0x55, 0xAA]),
            checksum,
            checksum_scope: ChecksumScope::HeaderAndPayload,
            checksum_endian: Endianness::Big,
            has_hw_tick_first_col: true,
            fields: fields.clone(),
            max_carry_bytes: 8192,
        };
        let mut codec = FixedSchemaBinaryCodec::new(cfg).unwrap();
        assert_eq!(codec.declared_channels().len(), 14);
        assert_eq!(
            codec.declared_channels()[6].kind,
            ChannelKind::Scalar {
                scalar_type: ScalarType::I16,
                calibration: LinearScale::new(0.01, -1.0),
            }
        );
        assert_eq!(
            codec.declared_channels()[13].kind,
            ChannelKind::Event {
                schema: EventSchemaKind::StateMachine {
                    labels: state_labels.clone(),
                },
            }
        );

        let mut payload = Vec::new();
        payload.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        payload.push(1); // b_flag
        payload.push(200); // u8_v
        payload.extend_from_slice(&0xBEEFu16.to_be_bytes());
        payload.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        payload.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        payload.push((-42i8) as u8);
        payload.extend_from_slice(&(-1234i16).to_le_bytes());
        payload.extend_from_slice(&(-987_654i32).to_be_bytes());
        payload.extend_from_slice(&(-123_456_789_012i64).to_be_bytes());
        payload.extend_from_slice(&(-12.5f32).to_be_bytes());
        payload.extend_from_slice(&core::f64::consts::PI.to_le_bytes());
        payload.push(0b1110_1000); // bit3=1 -> bf_bool=true; upper nibble 0b1110 -> signed -2
        payload.push(5); // fsm = ACTIVE
        assert_eq!(payload.len(), 49);

        let mut frame = vec![0xA5, 0x5A];
        frame.extend_from_slice(&payload);
        let cs = compute_checksum(checksum, &frame);
        match checksum {
            ChecksumKind::None => {}
            ChecksumKind::Sum8 | ChecksumKind::Xor8 => frame.push(cs as u8),
            ChecksumKind::Crc16Ccitt => frame.extend_from_slice(&(cs as u16).to_be_bytes()),
            ChecksumKind::Crc32 => frame.extend_from_slice(&cs.to_be_bytes()),
        }
        frame.extend_from_slice(&[0x55, 0xAA]);

        // Prepend false header + garbage and a corrupted checksum frame (when checksum != None).
        let mut wire = vec![0xA5, 0x5A, 0x00, 0xFF];
        if checksum != ChecksumKind::None {
            let mut bad = frame.clone();
            bad[10] ^= 0x01;
            wire.extend_from_slice(&bad);
        }
        wire.extend_from_slice(&frame);

        let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 4);
        for &b in &wire {
            codec.feed_bytes(&[b], 999, &mut batch).unwrap();
        }
        assert_eq!(batch.sample_rows, 1);
        assert_eq!(batch.hw_ticks, vec![0x1122_3344]);
        assert!(matches!(batch.columns[0], DecodedColumnBuf::Bool(ref v) if v == &[true]));
        assert!(matches!(batch.columns[1], DecodedColumnBuf::U8(ref v) if v == &[200]));
        assert!(matches!(batch.columns[2], DecodedColumnBuf::U16(ref v) if v == &[0xBEEF]));
        assert!(matches!(batch.columns[3], DecodedColumnBuf::U32(ref v) if v == &[0xCAFE_BABE]));
        assert!(
            matches!(batch.columns[4], DecodedColumnBuf::U64(ref v) if v == &[0x0102_0304_0506_0708])
        );
        assert!(matches!(batch.columns[5], DecodedColumnBuf::I8(ref v) if v == &[-42]));
        assert!(matches!(batch.columns[6], DecodedColumnBuf::I16(ref v) if v == &[-1234]));
        assert!(matches!(batch.columns[7], DecodedColumnBuf::I32(ref v) if v == &[-987_654]));
        assert!(
            matches!(batch.columns[8], DecodedColumnBuf::I64(ref v) if v == &[-123_456_789_012])
        );
        assert!(matches!(batch.columns[9], DecodedColumnBuf::F32(ref v) if v == &[-12.5]));
        assert!(
            matches!(batch.columns[10], DecodedColumnBuf::F64(ref v) if (v[0] - core::f64::consts::PI).abs() < 1e-15)
        );
        assert!(matches!(batch.columns[11], DecodedColumnBuf::Bool(ref v) if v == &[true]));
        assert!(matches!(batch.columns[12], DecodedColumnBuf::I8(ref v) if v == &[-2]));
        assert_eq!(batch.columns[13].as_state_slice(), Some(&[5][..]));
    }
}

#[test]
fn sim_preset_wire_roundtrip_and_garbage_resync() {
    let mut codec = SimPresetCodec::new(SimPresetConfig {
        source_id: "sim0".into(),
        analog_channels: 4,
        max_carry_bytes: 4096,
    })
    .unwrap();
    assert_eq!(codec.frame_bytes(), 28);

    let mut frame = Vec::new();
    encode_sim_preset_frame(
        0x0102_0304,
        &[1.5, -2.5, 3.5, 104.25],
        sim_preset_flags(true, false, true, true),
        3,
        &mut frame,
    );
    assert_eq!(frame.len(), 28);
    assert_eq!(&frame[0..2], &SIM_PRESET_MAGIC);
    assert_eq!(frame[2], 4);
    assert_eq!(frame[3], 0b1000_0101);
    assert_eq!(&frame[4..8], &0x0102_0304u32.to_le_bytes());
    assert_eq!(frame[24], 3);
    assert_eq!(frame[25], 0);
    assert_eq!(&frame[26..28], &SIM_PRESET_TAIL);

    let mut wire = vec![0x00, 0xDD, 0x53, 0x05]; // fake header with wrong A=5
    wire.extend_from_slice(&[0xAA; 17]);
    wire.extend_from_slice(&frame);
    // Corrupt tail copy followed by valid frame.
    let mut bad_tail = frame.clone();
    bad_tail[27] = 0x00;
    wire.extend_from_slice(&bad_tail);
    encode_sim_preset_frame(
        0x0102_0305,
        &[-0.5, 0.5, 1.25, 24.0],
        sim_preset_flags(false, true, false, false),
        2,
        &mut wire,
    );

    let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 8);
    for &b in &wire {
        codec.feed_bytes(&[b], 88_000, &mut batch).unwrap();
    }

    assert_eq!(batch.sample_rows, 2);
    assert_eq!(batch.hw_ticks, vec![0x0102_0304, 0x0102_0305]);
    assert_eq!(
        batch.columns[3].as_scalar_slice().unwrap().get_as_f64(0),
        Some(104.25)
    );
    assert!(matches!(batch.columns[4], DecodedColumnBuf::Bool(ref v) if v == &[true, false]));
    assert!(matches!(batch.columns[5], DecodedColumnBuf::Bool(ref v) if v == &[false, true]));
    assert!(matches!(batch.columns[6], DecodedColumnBuf::Bool(ref v) if v == &[true, false]));
    assert_eq!(batch.columns[7].as_state_slice(), Some(&[3, 2][..]));
}

#[test]
fn firewater_csv_typed_columns_tags_crlf_and_malformed_recovery() {
    let mut custom_labels = StateEnumMap::new();
    custom_labels.insert(
        10,
        StateLabel {
            name: "BOOST".into(),
            color_rgba: None,
        },
    );

    let cfg = FireWaterConfig::with_columns(
        "uart1",
        vec![
            FireWaterColumnSpec::f32("ia"),
            FireWaterColumnSpec::f64("vbus"),
            FireWaterColumnSpec::bool("pwm"),
            FireWaterColumnSpec::state_enum("state", custom_labels),
        ],
    );
    let mut codec = FireWaterCsvCodec::new(cfg).unwrap();
    let mut batch = DecodedBatch::from_descriptors(codec.declared_channels(), 16);

    let stream = concat!(
        "motor: 1.25, 24.1234567890123456789, TRUE, BOOST\r\n",
        "bad_too_few, 1.0\n",
        "1.0, 2.0, 1, 0, extra_col\n",
        "NaN, 24.0, 1, IDLE\n",
        "1.0, inf, 0, ALIGN\n",
        "1.0, 24.0, maybe, RUN\n",
        "1.0, 24.0, 1, UNKNOWN_STATE\n",
        "-2.5e-2, -0.0, false, CLOSED_LOOP\n",
        "diag: +3.5, 1e3, 1, OCP_FAULT\r\n",
        "0.5, 12.25, 0, 2\n",
    );

    // Arbitrary small chunks crossing tag, CRLF, and field boundaries.
    for chunk in stream.as_bytes().chunks(5) {
        codec.feed_bytes(chunk, 4444, &mut batch).unwrap();
    }

    assert_eq!(batch.sample_rows, 4);
    assert_eq!(codec.stats().malformed_lines, 6);
    assert_eq!(
        batch.columns[0].as_scalar_slice().unwrap().get_as_f64(0),
        Some(1.25)
    );
    assert_eq!(
        batch.columns[0].as_scalar_slice().unwrap().get_as_f64(1),
        Some(-0.025_f32 as f64)
    );
    assert!(
        (batch.columns[1]
            .as_scalar_slice()
            .unwrap()
            .get_as_f64(0)
            .unwrap()
            - 24.123_456_789_012_344)
            .abs()
            < 1e-13
    );
    assert!(matches!(
        batch.columns[2],
        DecodedColumnBuf::Bool(ref v) if v == &[true, false, true, false]
    ));
    assert_eq!(batch.columns[3].as_state_slice(), Some(&[10, 2, 3, 2][..]));
}

#[test]
fn downlink_encoding_and_protocol_registry_roundtrip() {
    let reg = ProtocolRegistry::with_builtin_codecs();
    assert_eq!(
        reg.registered_codecs(),
        vec![
            "fire_water",
            "fixed_schema_binary",
            "just_float",
            "sim_preset",
        ]
    );

    let mut codecs: Vec<Box<dyn ProtocolCodec>> = vec![
        reg.create("sim_preset", "{}").unwrap(),
        reg.create("just_float", r#"{"channel_names":["ch0","ch1"]}"#)
            .unwrap(),
        reg.create(
            "fixed_schema_binary",
            r#"{"header_magic":[170,85],"fields":[{"name":"vbus","scalar_type":"f32","scale":1.0,"offset":0.0,"unit":"V","is_state_enum":false}]}"#,
        )
        .unwrap(),
        reg.create("fire_water", r#"{"channel_names":["ch0","ch1"]}"#)
            .unwrap(),
    ];

    let mut out = BytesMut::new();
    for codec in &mut codecs {
        out.clear();
        codec
            .encode_downlink(&DownlinkPayload::RawBytes(vec![0x01, 0x02]), &mut out)
            .unwrap();
        assert_eq!(&out[..], &[0x01, 0x02]);

        out.clear();
        codec
            .encode_downlink(&DownlinkPayload::AsciiLine("GLITCH".into()), &mut out)
            .unwrap();
        assert_eq!(&out[..], b"GLITCH\n");

        out.clear();
        codec
            .encode_downlink(
                &DownlinkPayload::KeyValue {
                    key: "RATE".into(),
                    value: ScalarValue::U32(500_000),
                },
                &mut out,
            )
            .unwrap();
        assert_eq!(&out[..], b"RATE=500000\n");

        out.clear();
        codec
            .encode_downlink(
                &DownlinkPayload::CommandFields {
                    command_id: "SET".into(),
                    fields: vec![("kp".into(), ScalarValue::F64(0.75))],
                },
                &mut out,
            )
            .unwrap();
        assert_eq!(&out[..], b"SET kp=0.75\n");
    }

    let mut hex_out = BytesMut::new();
    encode_hex_string("0xDE 0xAD be ef 0x00ff", &mut hex_out).unwrap();
    assert_eq!(&hex_out[..], &[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF]);

    let standalone = BasicDownlinkEncoder::default();
    hex_out.clear();
    standalone
        .encode(&DownlinkPayload::AsciiLine("PING\n".into()), &mut hex_out)
        .unwrap();
    assert_eq!(&hex_out[..], b"PING\n");
}

#[test]
fn single_core_throughput_exceeds_10m_pts_per_sec() {
    // 1. JustFloat throughput (4 channels)
    let mut jf = JustFloatCodec::new(JustFloatConfig::new(
        "bench",
        vec!["c0".into(), "c1".into(), "c2".into(), "c3".into()],
    ))
    .unwrap();
    let jf_frames = 250_000usize;
    let mut jf_buf = Vec::with_capacity(jf_frames * 20);
    for i in 0..jf_frames {
        let v = (i & 1023) as f32 * 0.01;
        encode_justfloat_frame(&[v, -v, v * 2.0, 24.0], &mut jf_buf);
    }
    let mut jf_batch = DecodedBatch::from_descriptors(jf.declared_channels(), jf_frames);
    let jf_iters = if cfg!(debug_assertions) { 1 } else { 10 };
    let t0 = Instant::now();
    let mut jf_pts = 0usize;
    for _ in 0..jf_iters {
        jf_batch.clear();
        let rows = jf.feed_bytes(&jf_buf, 1, &mut jf_batch).unwrap();
        jf_pts += rows * 4;
    }
    let jf_secs = t0.elapsed().as_secs_f64().max(1e-9);
    let jf_rate = jf_pts as f64 / jf_secs;
    println!("JustFloat single-core throughput: {:.2} M pts/s", jf_rate / 1e6);

    // 2. SimPreset throughput (8 decoded channel columns: 4 f32 + 3 bool + 1 state)
    let mut sim = SimPresetCodec::new(SimPresetConfig::default()).unwrap();
    let sim_frames = 200_000usize;
    let mut sim_buf = Vec::with_capacity(sim_frames * 28);
    for i in 0..sim_frames {
        encode_sim_preset_frame(
            i as u32,
            &[1.0, -1.0, 0.5, 24.0],
            sim_preset_flags(i % 2 == 0, i % 3 == 0, false, false),
            (i % 4) as u8,
            &mut sim_buf,
        );
    }
    let mut sim_batch = DecodedBatch::from_descriptors(sim.declared_channels(), sim_frames);
    let sim_iters = if cfg!(debug_assertions) { 1 } else { 10 };
    let t1 = Instant::now();
    let mut sim_pts = 0usize;
    for _ in 0..sim_iters {
        sim_batch.clear();
        let rows = sim.feed_bytes(&sim_buf, 1, &mut sim_batch).unwrap();
        sim_pts += rows * 8;
    }
    let sim_secs = t1.elapsed().as_secs_f64().max(1e-9);
    let sim_rate = sim_pts as f64 / sim_secs;
    println!("SimPreset single-core throughput: {:.2} M pts/s", sim_rate / 1e6);

    // 3. FireWater CSV throughput (4 F32 channels)
    let mut fw = FireWaterCsvCodec::new(FireWaterConfig::from_channel_names(
        "bench",
        vec!["c0".into(), "c1".into(), "c2".into(), "c3".into()],
    ))
    .unwrap();
    let fw_lines = if cfg!(debug_assertions) { 20_000 } else { 250_000 };
    let mut fw_buf = Vec::with_capacity(fw_lines * 32);
    for i in 0..fw_lines {
        use std::io::Write as _;
        let _ = writeln!(fw_buf, "{}.25,-1.5,2.75,24.05", i % 1000);
    }
    let mut fw_batch = DecodedBatch::from_descriptors(fw.declared_channels(), fw_lines);
    let fw_iters = if cfg!(debug_assertions) { 1 } else { 5 };
    let t2 = Instant::now();
    let mut fw_pts = 0usize;
    for _ in 0..fw_iters {
        fw_batch.clear();
        let rows = fw.feed_bytes(&fw_buf, 1, &mut fw_batch).unwrap();
        fw_pts += rows * 4;
    }
    let fw_secs = t2.elapsed().as_secs_f64().max(1e-9);
    let fw_rate = fw_pts as f64 / fw_secs;
    println!("FireWater CSV single-core throughput: {:.2} M pts/s", fw_rate / 1e6);

    assert!(jf_pts > 0 && sim_pts > 0 && fw_pts > 0);
    #[cfg(not(debug_assertions))]
    {
        assert!(
            jf_rate > 10_000_000.0,
            "JustFloat throughput {jf_rate:.0} pts/s must exceed 10M pts/s"
        );
        assert!(
            sim_rate > 10_000_000.0,
            "SimPreset throughput {sim_rate:.0} pts/s must exceed 10M pts/s"
        );
        assert!(
            fw_rate > 10_000_000.0,
            "FireWater CSV throughput {fw_rate:.0} pts/s must exceed 10M pts/s"
        );
    }
}
