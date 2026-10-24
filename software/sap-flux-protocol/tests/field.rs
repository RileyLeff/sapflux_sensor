//! Field-mode message tests (docs/field-protocol.md §1): roundtrips, exact
//! payload sizes, wrong-length rejection, helper math, and the golden vector
//! file `tests/vectors/field_v1.txt`.
//!
//! Regenerate the vector file after an intentional wire change with:
//!
//! ```sh
//! SAP_FLUX_UPDATE_VECTORS=1 cargo test --test field
//! ```

use sap_flux_protocol::field::*;
use sap_flux_protocol::*;
use std::fmt::Write as _;

const DEFAULT_SCHEDULE: Schedule = Schedule {
    enabled: 1,
    ..Schedule::FACTORY_DEFAULT
};

fn sample_window_data() -> (WindowInfo, Vec<u8>) {
    // 5 bins × 13 channels (130 B, two chunks) with a mix of positive,
    // negative, clamped and missing values.
    let base_raw: [i16; 13] = [
        12000, 12010, 12020, 12030, 12040, 12050, 13000, 13010, 13020, 13030, 13040, 13050, 26100,
    ];
    let mut data = Vec::new();
    for bin in 0..5i16 {
        for c in 0..13usize {
            let v = match (bin, c) {
                (0, _) => 0,
                (1, 3) => BIN_VALUE_MISSING,
                (2, 0) => i16::MAX,
                (2, 1) => -i16::MAX,
                _ => bin * 100 + c as i16 * 3 - 20,
            };
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    let info = WindowInfo {
        run_id: 17,
        window_flags: WINDOW_FLAG_ADC_ERRORS,
        channels: 13,
        bin_ms_div10: 50,
        n_bins: 5,
        chunk_count: chunk_count(data.len()) as u16,
        start_monotonic_us: 3_600_000_000,
        start_rtc_unix_ms: 1_793_000_000_000,
        end_monotonic_us: 3_602_500_000,
        end_rtc_unix_ms: 1_793_000_002_500,
        heater_on_offset_ms: 500,
        pulse_duration_ms: 1000,
        sample_hz: 25,
        v24_mean_raw: 21000,
        frames_total: 62,
        frames_missing: 1,
        data_crc32: crc::crc32(&data),
        base_raw,
    };
    (info, data)
}

/// One instance of every field message, with names for the vector file.
fn field_messages() -> Vec<(&'static str, u32, Message)> {
    let (info, data) = sample_window_data();
    let last = info.chunk_count - 1;
    let last_range = chunk_range(last, data.len()).unwrap();
    vec![
        (
            "SetSchedule",
            1,
            Message::SetSchedule(SetSchedule {
                command_nonce: 0x0102_0304_0506_0708,
                schedule: DEFAULT_SCHEDULE,
                lease_until_unix_s: 1_793_003_600,
            }),
        ),
        (
            "SetSchedule_require_lease",
            2,
            Message::SetSchedule(SetSchedule {
                command_nonce: 9,
                schedule: Schedule {
                    flags: SCHEDULE_FLAG_REQUIRE_LEASE,
                    pulse_duration_ms: 1500,
                    interval_s: 7200,
                    phase_s: 900,
                    baseline_s: 30,
                    recovery_s: 300,
                    sample_hz: 10,
                    bin_ms: 1000,
                    ..DEFAULT_SCHEDULE
                },
                lease_until_unix_s: 0xFFFF_FFFF,
            }),
        ),
        (
            "GetWindow_header_oldest",
            3,
            Message::GetWindow(GetWindow {
                command_nonce: 10,
                run_id: WINDOW_OLDEST,
                chunk: CHUNK_HEADER,
            }),
        ),
        (
            "GetWindow_chunk",
            4,
            Message::GetWindow(GetWindow {
                command_nonce: 11,
                run_id: 17,
                chunk: 125,
            }),
        ),
        (
            "AckWindow",
            5,
            Message::AckWindow(AckWindow {
                command_nonce: 12,
                run_id: 17,
            }),
        ),
        (
            "GetFieldStatus",
            6,
            Message::GetFieldStatus(GetFieldStatus { command_nonce: 13 }),
        ),
        (
            "Sample12",
            7,
            Message::Sample12(Sample12 {
                monotonic_us: 123_456_789_012,
                rtc_unix_ms: 1_793_000_000_123,
                run_id: 17,
                flags: FLAG_RTC_VALID | FLAG_PGOOD | FLAG_EXCITATION_ACTIVE,
                state: BoardState::Recovery,
                excitation_raw: 26100,
                v24_raw: 21000,
                thermistor_raw: [
                    12000, 12010, 12020, 12030, 12040, 12050, 13000, 13010, 13020, 13030, -1,
                    -32768,
                ],
            }),
        ),
        ("WindowInfo", 8, Message::WindowInfo(info)),
        (
            "WindowChunk_0",
            9,
            Message::WindowChunk(WindowChunk::new(17, 0, &data[..100]).unwrap()),
        ),
        (
            "WindowChunk_last",
            10,
            Message::WindowChunk(WindowChunk::new(17, last, &data[last_range]).unwrap()),
        ),
        (
            "FieldStatus",
            11,
            Message::FieldStatus(FieldStatus {
                monotonic_us: 7_200_000_000,
                rtc_unix_ms: 1_793_000_100_000,
                next_pulse_unix_s: 1_793_003_600,
                schedule_flags: FIELD_STATUS_ENABLED
                    | FIELD_STATUS_RTC_VALID
                    | FIELD_STATUS_FLASH_OK,
                windows_pending: 2,
                windows_dropped: 1,
                oldest_pending_run_id: 16,
                last_run_id: 17,
                calr_ppb: -12_345,
                last_rtc_step_ms: -310,
                boot_count: 4,
                v24_raw: 21000,
                flash_used_kib: 2048,
                schedule_crc: DEFAULT_SCHEDULE.crc16(),
            }),
        ),
        (
            "Ack_not_found",
            12,
            Message::Ack(Ack {
                command_type: MSG_GET_WINDOW,
                result: ACK_NOT_FOUND,
                command_nonce: 11,
                run_id: 17,
            }),
        ),
    ]
}

fn encode_to_vec(msg: &Message, seq: u32) -> Vec<u8> {
    let mut buf = [0u8; MAX_ENCODED_LEN];
    let n = encode(msg, seq, &mut buf).expect("encode");
    buf[..n].to_vec()
}

fn frame_to_vec(msg: &Message, seq: u32) -> Vec<u8> {
    let mut buf = [0u8; MAX_FRAME_LEN];
    let n = encode_frame(msg, seq, &mut buf).expect("encode_frame");
    buf[..n].to_vec()
}

fn feed(dec: &mut Decoder, bytes: &[u8]) -> Vec<Result<Frame, DecodeError>> {
    bytes.iter().filter_map(|&b| dec.push(b)).collect()
}

fn build_raw_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![PROTOCOL_VERSION, msg_type];
    f.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(payload);
    let crc = crc::crc32(&f);
    f.extend_from_slice(&crc.to_le_bytes());
    f
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[test]
fn roundtrip_every_field_message() {
    let mut dec = Decoder::new();
    for (name, seq, msg) in field_messages() {
        let wire = encode_to_vec(&msg, seq);
        assert!(wire.len() <= MAX_ENCODED_LEN, "{name}");
        assert!(
            !wire[..wire.len() - 1].contains(&0),
            "{name}: zero-free body"
        );
        let frames = feed(&mut dec, &wire);
        assert_eq!(
            frames,
            vec![Ok(Frame {
                sequence: seq,
                message: msg
            })],
            "{name}"
        );
        // decode_frame on the un-stuffed bytes agrees.
        let raw = frame_to_vec(&msg, seq);
        assert_eq!(
            decode_frame(&raw),
            Ok(Frame {
                sequence: seq,
                message: msg
            }),
            "{name}"
        );
    }
}

#[test]
fn exact_payload_sizes() {
    let expected = [
        (MSG_SET_SCHEDULE, 32),
        (MSG_GET_WINDOW, 14),
        (MSG_ACK_WINDOW, 12),
        (MSG_GET_FIELD_STATUS, 8),
        (MSG_SAMPLE12, 52),
        (MSG_WINDOW_INFO, 90),
        (MSG_WINDOW_CHUNK, 108),
        (MSG_FIELD_STATUS, 52),
    ];
    for (t, n) in expected {
        assert_eq!(payload_len_for(t), Some(n), "type {t:#04x}");
    }
    for (name, seq, msg) in field_messages() {
        let raw = frame_to_vec(&msg, seq);
        assert_eq!(raw.len(), MIN_FRAME_LEN + msg.payload_len(), "{name}");
        assert_eq!(payload_len_for(msg.message_type()), Some(msg.payload_len()));
        assert!(raw.len() <= MAX_FRAME_LEN, "{name}");
    }
    assert_eq!(WINDOW_INFO_FRAME_LEN, 102);
    // Largest frame overall is WindowChunk: 120 decoded bytes.
    let max = (0u8..=0xFF).filter_map(payload_len_for).max().unwrap();
    assert_eq!(max, 108);
    assert!(MIN_FRAME_LEN + max <= MAX_FRAME_LEN);
}

#[test]
fn type_codes_match_spec() {
    assert_eq!(MSG_SET_SCHEDULE, 0x07);
    assert_eq!(MSG_GET_WINDOW, 0x08);
    assert_eq!(MSG_ACK_WINDOW, 0x09);
    assert_eq!(MSG_GET_FIELD_STATUS, 0x0A);
    assert_eq!(MSG_SAMPLE12, 0x15);
    assert_eq!(MSG_WINDOW_INFO, 0x16);
    assert_eq!(MSG_WINDOW_CHUNK, 0x17);
    assert_eq!(MSG_FIELD_STATUS, 0x18);
    assert_eq!(ACK_NOT_FOUND, 0x04);
    assert_eq!(ACK_INVALID_PARAMETER, 0x02);
}

#[test]
fn wrong_lengths_rejected() {
    for (t, n) in [
        (MSG_SET_SCHEDULE, 32usize),
        (MSG_GET_WINDOW, 14),
        (MSG_ACK_WINDOW, 12),
        (MSG_GET_FIELD_STATUS, 8),
        (MSG_SAMPLE12, 52),
        (MSG_WINDOW_INFO, 90),
        (MSG_WINDOW_CHUNK, 108),
        (MSG_FIELD_STATUS, 52),
    ] {
        for bad in [0, n - 1, n + 1, MAX_PAYLOAD_LEN] {
            if bad == n {
                continue;
            }
            let raw = build_raw_frame(t, &vec![0u8; bad]);
            assert_eq!(
                decode_frame(&raw),
                Err(DecodeError::PayloadLength {
                    message_type: t,
                    expected: n,
                    actual: bad
                }),
                "type {t:#04x} len {bad}"
            );
        }
    }
}

#[test]
fn sample12_invalid_state_rejected_and_reserved_ignored() {
    let mut payload = [0u8; 52];
    payload[22] = 7; // state
    assert_eq!(
        decode_frame(&build_raw_frame(MSG_SAMPLE12, &payload)),
        Err(DecodeError::InvalidState(7))
    );
    payload[22] = BoardState::Pulsing as u8;
    payload[23] = 0xAA; // reserved
    match decode_frame(&build_raw_frame(MSG_SAMPLE12, &payload)) {
        Ok(Frame {
            message: Message::Sample12(s),
            ..
        }) => assert_eq!(s.state, BoardState::Pulsing),
        other => panic!("{other:?}"),
    }
}

#[test]
fn sample12_field_order() {
    let msg = Message::Sample12(Sample12 {
        monotonic_us: 1,
        rtc_unix_ms: 2,
        run_id: 3,
        flags: 4,
        state: BoardState::Armed,
        excitation_raw: 0x1111,
        v24_raw: 0x2222,
        thermistor_raw: core::array::from_fn(|i| 0x3300 + i as i16),
    });
    let raw = frame_to_vec(&msg, 0);
    let p = &raw[HEADER_LEN..raw.len() - CRC_LEN];
    assert_eq!(p[22], 2);
    assert_eq!(p[23], 0);
    assert_eq!(&p[24..26], &0x1111i16.to_le_bytes());
    assert_eq!(&p[26..28], &0x2222i16.to_le_bytes());
    assert_eq!(&p[28..30], &0x3300i16.to_le_bytes());
    assert_eq!(&p[50..52], &0x330Bi16.to_le_bytes());
}

#[test]
fn window_chunk_len_over_100_rejected() {
    let mut payload = [0u8; 108];
    payload[6..8].copy_from_slice(&101u16.to_le_bytes());
    assert_eq!(
        decode_frame(&build_raw_frame(MSG_WINDOW_CHUNK, &payload)),
        Err(DecodeError::ChunkLength(101))
    );
    // Padding bytes are ignored (normalized to zero) on receive.
    payload[6..8].copy_from_slice(&3u16.to_le_bytes());
    payload[8..11].copy_from_slice(&[1, 2, 3]);
    payload[50] = 0x77;
    match decode_frame(&build_raw_frame(MSG_WINDOW_CHUNK, &payload)) {
        Ok(Frame {
            message: Message::WindowChunk(c),
            ..
        }) => {
            assert_eq!(c.bytes(), &[1, 2, 3]);
            assert_eq!(c, WindowChunk::new(0, 0, &[1, 2, 3]).unwrap());
        }
        other => panic!("{other:?}"),
    }
    assert!(WindowChunk::new(0, 0, &[0u8; 101]).is_none());
}

#[test]
fn window_info_and_field_status_reserved_ignored() {
    let mut payload = [0u8; 90];
    payload[88] = 0xEE;
    payload[89] = 0xEE;
    assert!(matches!(
        decode_frame(&build_raw_frame(MSG_WINDOW_INFO, &payload)),
        Ok(Frame {
            message: Message::WindowInfo(_),
            ..
        })
    ));
    let mut payload = [0u8; 52];
    payload[50] = 0xEE;
    assert!(matches!(
        decode_frame(&build_raw_frame(MSG_FIELD_STATUS, &payload)),
        Ok(Frame {
            message: Message::FieldStatus(_),
            ..
        })
    ));
}

#[test]
fn schedule_bytes_and_crc() {
    let s = DEFAULT_SCHEDULE;
    let b = s.to_bytes();
    assert_eq!(b.len(), SCHEDULE_LEN);
    assert_eq!(Schedule::from_bytes(&b), s);
    assert_eq!(s.crc16(), crc::crc32(&b) as u16);
    // The schedule bytes are exactly SetSchedule payload bytes 8..28.
    let msg = Message::SetSchedule(SetSchedule {
        command_nonce: 0xAABB,
        schedule: s,
        lease_until_unix_s: 5,
    });
    let raw = frame_to_vec(&msg, 0);
    assert_eq!(&raw[HEADER_LEN + 8..HEADER_LEN + 28], &b);
    // Changing the lease or nonce does not change the CRC.
    let other = SetSchedule {
        command_nonce: 1,
        schedule: s,
        lease_until_unix_s: 99,
    };
    assert_eq!(other.schedule.crc16(), s.crc16());
    assert_eq!(Schedule::default(), Schedule::FACTORY_DEFAULT);
    assert!(!Schedule::FACTORY_DEFAULT.is_enabled());
}

#[test]
fn schedule_limits() {
    let cooldown = 600_000;
    assert_eq!(DEFAULT_SCHEDULE.validate(cooldown), Ok(()));
    assert_eq!(Schedule::FACTORY_DEFAULT.validate(cooldown), Ok(()));
    let bad = |f: fn(&mut Schedule)| {
        let mut s = DEFAULT_SCHEDULE;
        f(&mut s);
        s.validate(cooldown)
    };
    assert_eq!(bad(|s| s.enabled = 2), Err(ScheduleError::Enabled));
    assert_eq!(bad(|s| s.flags = 2), Err(ScheduleError::Flags));
    assert_eq!(
        bad(|s| s.pulse_duration_ms = 750),
        Err(ScheduleError::PulseDuration)
    );
    assert_eq!(bad(|s| s.interval_s = 599), Err(ScheduleError::Interval));
    assert_eq!(bad(|s| s.baseline_s = 9), Err(ScheduleError::Baseline));
    assert_eq!(bad(|s| s.baseline_s = 301), Err(ScheduleError::Baseline));
    assert_eq!(bad(|s| s.recovery_s = 59), Err(ScheduleError::Recovery));
    assert_eq!(bad(|s| s.recovery_s = 601), Err(ScheduleError::Recovery));
    assert_eq!(bad(|s| s.sample_hz = 50), Err(ScheduleError::SampleHz));
    assert_eq!(bad(|s| s.bin_ms = 100), Err(ScheduleError::BinMs));
    // 60 + 180 + 600 = 840 s busy: interval 840 rejected, 841 accepted.
    assert_eq!(bad(|s| s.interval_s = 840), Err(ScheduleError::Timing));
    assert_eq!(bad(|s| s.interval_s = 841), Ok(()));
    assert_eq!(bad(|s| s.flags = SCHEDULE_FLAG_REQUIRE_LEASE), Ok(()));
    // Short-cooldown dev builds make the 600 s floor reachable.
    let mut s = DEFAULT_SCHEDULE;
    s.interval_s = 600;
    assert_eq!(s.validate(30_000), Ok(()));
}

#[test]
fn default_window_size_matches_spec() {
    // (60 + 1 + 180) s at 500 ms bins = 482 bins, 12 532 B, 126 chunks.
    let bins = DEFAULT_SCHEDULE.window_bins();
    assert_eq!(bins, 482);
    let len = window_data_len(bins as u16, WINDOW_CHANNELS as u8);
    assert_eq!(len, 12_532);
    assert_eq!(chunk_count(len), 126);
    assert_eq!(chunk_range(0, len), Some(0..100));
    assert_eq!(chunk_range(125, len), Some(12_500..12_532));
    assert_eq!(chunk_range(126, len), None);
    assert_eq!(chunk_count(0), 0);
    assert_eq!(chunk_range(0, 0), None);
    assert_eq!(chunk_count(100), 1);
    assert_eq!(chunk_count(101), 2);
}

#[test]
fn bin_value_encoding() {
    // Exact mean equal to base -> 0.
    assert_eq!(encode_bin_value(12000 * 12, 12, 12000), 0);
    // Mean 12000.5 -> +8.
    assert_eq!(encode_bin_value(24001, 2, 12000), 8);
    // 1/32 count above base: 16/32 = 0.5 -> rounds away from zero to 1.
    assert_eq!(encode_bin_value(12000 * 32 + 1, 32, 12000), 1);
    assert_eq!(encode_bin_value(12000 * 32 - 1, 32, 12000), -1);
    // Just below half rounds toward zero.
    assert_eq!(encode_bin_value(12000 * 33 + 1, 33, 12000), 0);
    // Clamping never produces the sentinel.
    assert_eq!(encode_bin_value(32767, 1, -32768), i16::MAX);
    assert_eq!(encode_bin_value(-32768, 1, 32767), -i16::MAX);
    assert_eq!(encode_bin_value(0, 0, 0), BIN_VALUE_MISSING);
    // Decoding.
    assert_eq!(decode_bin_value(8, 12000), Some(12000.5));
    assert_eq!(decode_bin_value(-16, 100), Some(99.0));
    assert_eq!(decode_bin_value_x16(-1, 100), Some(1599));
    assert_eq!(decode_bin_value(BIN_VALUE_MISSING, 100), None);
    // Base rounding.
    assert_eq!(mean_raw_rounded(25, 2), Some(13));
    assert_eq!(mean_raw_rounded(-25, 2), Some(-13));
    assert_eq!(mean_raw_rounded(24, 2), Some(12));
    assert_eq!(mean_raw_rounded(0, 0), None);
    // Round trip within 1/32 count for random-ish sums.
    for (sum, count, base) in [(300_123, 25, 12000), (-50_001, 7, -7000), (1, 3, 0)] {
        let v = encode_bin_value(sum, count, base);
        let back = decode_bin_value(v, base).unwrap() as f64;
        let mean = sum as f64 / count as f64;
        assert!((back - mean).abs() <= 1.0 / 32.0 + 1e-9, "{sum}/{count}");
    }
}

#[test]
fn window_info_helpers() {
    let (info, data) = sample_window_data();
    assert_eq!(info.bin_ms(), 500);
    assert_eq!(info.data_len(), 130);
    assert_eq!(info.expected_chunk_count(), 2);
    assert_eq!(chunk_range(1, data.len()), Some(100..130));
    assert!(info.is_consistent());
    assert!(info.verify_data(&data));
    let mut bad = data.clone();
    bad[5] ^= 1;
    assert!(!info.verify_data(&bad));
    assert!(!info.verify_data(&data[..129]));
    assert_eq!(info.bin_start_monotonic_us(2), 3_601_000_000);
    assert_eq!(read_bin_value(&data, 1, 3, 13), Some(BIN_VALUE_MISSING));
    assert_eq!(read_bin_value(&data, 2, 0, 13), Some(i16::MAX));
    assert_eq!(read_bin_value(&data, 5, 0, 13), None);
    assert_eq!(read_bin_value(&data, 0, 13, 13), None);
}

#[test]
fn flag_constants_are_distinct_bits() {
    let w = [
        WINDOW_FLAG_TIME_INVALID,
        WINDOW_FLAG_ADC_ERRORS,
        WINDOW_FLAG_ABORTED,
        WINDOW_FLAG_HEATER_FAULT,
        WINDOW_FLAG_TRUNCATED,
    ];
    for (i, f) in w.iter().enumerate() {
        assert_eq!(*f, 1 << i);
    }
    let s = [
        FIELD_STATUS_ENABLED,
        FIELD_STATUS_REQUIRE_LEASE,
        FIELD_STATUS_LEASE_VALID,
        FIELD_STATUS_RTC_VALID,
        FIELD_STATUS_HEATER_LOCKOUT,
        FIELD_STATUS_FLASH_OK,
    ];
    for (i, f) in s.iter().enumerate() {
        assert_eq!(*f, 1 << i);
    }
    assert_eq!(SCHEDULE_FLAG_REQUIRE_LEASE, 1);
}

/// Text of the golden vector file. Format is documented in its header.
fn vector_file_text() -> String {
    let mut out = String::new();
    out.push_str(
        "# sap-flux protocol v1 field-mode test vectors (docs/field-protocol.md section 1).\n\
         # Generated by software/sap-flux-protocol/tests/field.rs; regenerate with\n\
         #   SAP_FLUX_UPDATE_VECTORS=1 cargo test --test field\n\
         # All hex is lowercase, no separators. Fields are separated by single spaces.\n\
         #\n\
         # frame <name> <sequence> <decoded_frame_hex> <wire_hex>\n\
         #   decoded_frame = header + payload + CRC-32 (no COBS); wire = COBS + 0x00 delimiter.\n\
         # schedule <name> <schedule_bytes_hex> <schedule_crc_u16_hex>\n\
         #   schedule_bytes = SetSchedule bytes enabled..bin_ms (20 B); crc = low 16 bits of CRC-32.\n\
         # window_data <name> <data_hex> <data_crc32_hex>\n\
         #   window data of the WindowInfo frame vector (bins x 13 x i16 LE).\n\
         # bin_value <sum> <count> <base_raw> <value>\n\
         #   value = clamp(round_half_away_from_zero(16*(sum/count - base_raw)), -32767, 32767);\n\
         #   count 0 -> -32768 (missing).\n",
    );
    for (name, seq, msg) in field_messages() {
        let raw = frame_to_vec(&msg, seq);
        let wire = encode_to_vec(&msg, seq);
        writeln!(out, "frame {name} {seq} {} {}", hex(&raw), hex(&wire)).unwrap();
    }
    for (name, s) in [
        ("factory_default", Schedule::FACTORY_DEFAULT),
        ("deployment_default", DEFAULT_SCHEDULE),
    ] {
        writeln!(
            out,
            "schedule {name} {} {:04x}",
            hex(&s.to_bytes()),
            s.crc16()
        )
        .unwrap();
    }
    let (info, data) = sample_window_data();
    writeln!(
        out,
        "window_data WindowInfo {} {:08x}",
        hex(&data),
        info.data_crc32
    )
    .unwrap();
    for (sum, count, base) in [
        (288_000i32, 24u32, 12000i16),
        (24_001, 2, 12000),
        (384_001, 32, 12000),
        (383_999, 32, 12000),
        (396_001, 33, 12000),
        (-50_001, 7, -7000),
        (32_767, 1, -32768),
        (-32_768, 1, 32767),
        (0, 0, 0),
    ] {
        writeln!(
            out,
            "bin_value {sum} {count} {base} {}",
            encode_bin_value(sum, count, base)
        )
        .unwrap();
    }
    out
}

#[test]
fn golden_vector_file() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/field_v1.txt");
    let text = vector_file_text();
    if std::env::var_os("SAP_FLUX_UPDATE_VECTORS").is_some() {
        std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors")).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    let on_disk = std::fs::read_to_string(path)
        .expect("vector file missing; run with SAP_FLUX_UPDATE_VECTORS=1");
    assert_eq!(on_disk, text, "wire encoding changed; see file header");

    // Every frame line decodes back through the streaming decoder.
    for line in on_disk.lines().filter(|l| l.starts_with("frame ")) {
        let f: Vec<&str> = line.split(' ').collect();
        let wire: Vec<u8> = (0..f[4].len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&f[4][i..i + 2], 16).unwrap())
            .collect();
        let mut dec = Decoder::new();
        let frames = feed(&mut dec, &wire);
        assert_eq!(frames.len(), 1, "{line}");
        assert!(frames[0].is_ok(), "{line}");
        assert_eq!(frames[0].unwrap().sequence, f[2].parse::<u32>().unwrap());
    }
}
