//! Exhaustive protocol tests: roundtrips, golden bytes, corruption,
//! truncation, malformed input, and deterministic fuzz-style streams.

use sap_flux_protocol::*;

fn all_messages() -> Vec<Message> {
    vec![
        Message::Hello(Hello {
            board_id: 0xDEAD_BEEF_CAFE_F00D,
            firmware_version: (1 << 16) | (2 << 8) | 3,
            capabilities: 5,
        }),
        Message::Sample(Sample {
            monotonic_us: 123_456_789_012,
            rtc_unix_ms: 1_772_000_000_123,
            run_id: 42,
            flags: FLAG_RTC_VALID | FLAG_PGOOD | FLAG_EXCITATION_ACTIVE,
            excitation_raw: 26123,
            thermistor_raw: [12000, -1, 0, 13000, 32767, -32768],
            v24_raw: 21000,
            state: BoardState::Baseline,
        }),
        Message::Status(Status {
            monotonic_us: u64::MAX,
            rtc_unix_ms: -1,
            run_id: u32::MAX,
            state: BoardState::Cooldown,
            fault_code: 9,
            pulse_duration_ms: 1500,
            cooldown_remaining_ms: 600_000,
        }),
        Message::Ack(Ack {
            command_type: MSG_ARM_PULSE,
            result: 0,
            command_nonce: u64::MAX,
            run_id: 7,
        }),
        Message::Error(ErrorReport {
            fault_code: 0xBEEF,
            detail: 0x1234,
        }),
        Message::Query(Query {
            command_nonce: 0x1122_3344_5566_7788,
        }),
        Message::SetRtc(SetRtc {
            command_nonce: 1,
            unix_ms: 1_772_000_000_000,
        }),
        Message::StartBaseline(StartBaseline {
            command_nonce: 2,
            run_id: 8,
            baseline_duration_ms: 60_000,
            recovery_duration_ms: 180_000,
            requested_sample_hz: 50,
        }),
        Message::ArmPulse(ArmPulse {
            command_nonce: 3,
            run_id: 8,
            pulse_duration_ms: 500,
        }),
        Message::FirePulse(FirePulse {
            command_nonce: 4,
            run_id: 8,
        }),
        Message::Abort(Abort {
            command_nonce: 5,
            run_id: 8,
        }),
    ]
}

fn encode_to_vec(msg: &Message, seq: u32) -> Vec<u8> {
    let mut buf = [0u8; MAX_ENCODED_LEN];
    let n = encode(msg, seq, &mut buf).expect("encode");
    buf[..n].to_vec()
}

fn feed(dec: &mut Decoder, bytes: &[u8]) -> Vec<Result<Frame, DecodeError>> {
    bytes.iter().filter_map(|&b| dec.push(b)).collect()
}

/// Build a decoded (pre-COBS) frame with arbitrary header/payload, valid CRC
/// unless `corrupt_crc`.
fn build_raw_frame(
    version: u8,
    msg_type: u8,
    declared_len: u16,
    seq: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut f = Vec::new();
    f.push(version);
    f.push(msg_type);
    f.extend_from_slice(&declared_len.to_le_bytes());
    f.extend_from_slice(&seq.to_le_bytes());
    f.extend_from_slice(payload);
    let crc = crc::crc32(&f);
    f.extend_from_slice(&crc.to_le_bytes());
    f
}

fn to_wire(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; raw.len() + raw.len() / 254 + 2];
    let n = cobs::encode(raw, &mut out).unwrap();
    out.truncate(n);
    out.push(0);
    out
}

#[test]
fn roundtrip_all_message_types() {
    let mut dec = Decoder::new();
    for (i, msg) in all_messages().into_iter().enumerate() {
        let seq = 0xA000_0000u32.wrapping_add(i as u32);
        let wire = encode_to_vec(&msg, seq);
        assert!(wire.len() <= MAX_ENCODED_LEN);
        assert_eq!(wire.last(), Some(&0u8), "delimiter-terminated");
        assert!(!wire[..wire.len() - 1].contains(&0), "zero-free body");
        let results = feed(&mut dec, &wire);
        assert_eq!(results.len(), 1, "exactly one frame for {msg:?}");
        let frame = results[0].expect("valid frame");
        assert_eq!(frame.sequence, seq);
        assert_eq!(frame.message, msg);
    }
}

#[test]
fn golden_query_bytes() {
    // Independently computed (Python: struct + zlib.crc32 + reference COBS).
    let expected: &[u8] = &[
        0x04, 0x01, 0x01, 0x08, 0x02, 0x01, 0x01, 0x01, 0x0D, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33,
        0x22, 0x11, 0x32, 0x7D, 0xC2, 0x2B, 0x00,
    ];
    let msg = Message::Query(Query {
        command_nonce: 0x1122_3344_5566_7788,
    });
    assert_eq!(encode_to_vec(&msg, 1), expected);
    let mut dec = Decoder::new();
    let frames = feed(&mut dec, expected);
    assert_eq!(
        frames,
        vec![Ok(Frame {
            sequence: 1,
            message: msg
        })]
    );
}

#[test]
fn golden_hello_bytes() {
    let expected: &[u8] = &[
        0x04, 0x01, 0x10, 0x10, 0x01, 0x01, 0x01, 0x01, 0x0C, 0x0D, 0xF0, 0xFE, 0xCA, 0xEF, 0xBE,
        0xAD, 0xDE, 0x03, 0x02, 0x01, 0x02, 0x05, 0x01, 0x01, 0x05, 0x72, 0x37, 0xC2, 0x1C, 0x00,
    ];
    let msg = Message::Hello(Hello {
        board_id: 0xDEAD_BEEF_CAFE_F00D,
        firmware_version: (1 << 16) | (2 << 8) | 3,
        capabilities: 5,
    });
    assert_eq!(encode_to_vec(&msg, 0), expected);
}

#[test]
fn golden_sample_bytes() {
    let expected: &[u8] = &[
        0x04, 0x01, 0x11, 0x28, 0x08, 0x04, 0x03, 0x02, 0x01, 0x40, 0x42, 0x0F, 0x01, 0x01, 0x01,
        0x01, 0x0A, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x07, 0x01, 0x01, 0x02, 0x07,
        0x08, 0x90, 0x65, 0xE0, 0x2E, 0xD4, 0x30, 0x03, 0x0A, 0xC8, 0x32, 0xBC, 0x34, 0xB0, 0x36,
        0x08, 0x52, 0x03, 0x05, 0xE8, 0xFA, 0x43, 0x31, 0x00,
    ];
    let msg = Message::Sample(Sample {
        monotonic_us: 1_000_000,
        rtc_unix_ms: -1,
        run_id: 7,
        flags: FLAG_HEATER_ACTIVE | FLAG_RTC_VALID | FLAG_PGOOD,
        excitation_raw: 26000,
        thermistor_raw: [12000, 12500, 3, 13000, 13500, 14000],
        v24_raw: 21000,
        state: BoardState::Pulsing,
    });
    assert_eq!(encode_to_vec(&msg, 0x0102_0304), expected);
    let mut dec = Decoder::new();
    let frames = feed(&mut dec, expected);
    assert_eq!(
        frames,
        vec![Ok(Frame {
            sequence: 0x0102_0304,
            message: msg
        })]
    );
}

#[test]
fn flag_and_capability_constants_are_distinct_bits() {
    let flags = [
        FLAG_HEATER_ACTIVE,
        FLAG_RTC_VALID,
        FLAG_PGOOD,
        FLAG_EXCITATION_ACTIVE,
        FLAG_ADC_ERROR,
        FLAG_COMMAND_WINDOW,
    ];
    for f in flags {
        assert_eq!(f.count_ones(), 1, "flag {f:#06x} is not a single bit");
    }
    assert_eq!(
        flags.iter().fold(0u16, |acc, f| acc | f).count_ones() as usize,
        flags.len(),
        "sample flag bits overlap"
    );
    assert_eq!(FLAG_COMMAND_WINDOW, 1 << 5);

    let caps = [CAP_RTC, CAP_ADC, CAP_V24_MONITOR, CAP_HEATER_CONTROL];
    for c in caps {
        assert_eq!(
            c.count_ones(),
            1,
            "capability {c:#010x} is not a single bit"
        );
    }
    assert_eq!(
        caps.iter().fold(0u32, |acc, c| acc | c).count_ones() as usize,
        caps.len(),
        "capability bits overlap"
    );
    assert_eq!(CAP_RTC, 1 << 0);
    assert_eq!(CAP_ADC, 1 << 1);
    assert_eq!(CAP_V24_MONITOR, 1 << 2);
    assert_eq!(CAP_HEATER_CONTROL, 1 << 3);
}

#[test]
fn command_window_flag_survives_sample_roundtrip() {
    let msg = Message::Sample(Sample {
        monotonic_us: 55,
        rtc_unix_ms: 1_772_000_000_000,
        run_id: 3,
        flags: FLAG_RTC_VALID | FLAG_PGOOD | FLAG_COMMAND_WINDOW,
        excitation_raw: 26000,
        thermistor_raw: [1, 2, 3, 4, 5, 6],
        v24_raw: 24000,
        state: BoardState::SafeIdle,
    });
    let wire = encode_to_vec(&msg, 12);
    let mut dec = Decoder::new();
    let frames = feed(&mut dec, &wire);
    assert_eq!(frames.len(), 1);
    let frame = frames[0].expect("valid frame");
    assert_eq!(frame.message, msg);
    match frame.message {
        Message::Sample(s) => {
            assert_ne!(s.flags & FLAG_COMMAND_WINDOW, 0, "bit 5 must survive");
        }
        other => panic!("expected sample, got {other:?}"),
    }
}

#[test]
fn crc_corruption_rejected_then_recovers() {
    let msg = all_messages()[1]; // Sample
    let good = encode_to_vec(&msg, 10);

    // Corrupt one payload byte pre-COBS so only the CRC check can catch it.
    let raw = build_raw_frame(1, MSG_QUERY, 8, 3, &7u64.to_le_bytes());
    let mut bad = raw.clone();
    bad[9] ^= 0x55; // payload byte
    let wire_bad = to_wire(&bad);

    let mut dec = Decoder::new();
    let results = feed(&mut dec, &wire_bad);
    assert_eq!(results.len(), 1);
    assert!(
        matches!(results[0], Err(DecodeError::Crc { .. })),
        "{:?}",
        results[0]
    );

    // Decoder must recover on the very next frame.
    let results = feed(&mut dec, &good);
    assert_eq!(
        results,
        vec![Ok(Frame {
            sequence: 10,
            message: msg
        })]
    );
}

#[test]
fn every_single_byte_corruption_is_rejected_or_detected() {
    // Flip each wire byte (except making new zeros, which just re-frames)
    // and require that we never accept a frame with wrong content.
    let msg = Message::StartBaseline(StartBaseline {
        command_nonce: 99,
        run_id: 4,
        baseline_duration_ms: 60_000,
        recovery_duration_ms: 180_000,
        requested_sample_hz: 50,
    });
    let wire = encode_to_vec(&msg, 77);
    for i in 0..wire.len() - 1 {
        for flip in [0x01u8, 0x80, 0xFF] {
            let mut w = wire.clone();
            w[i] ^= flip;
            let mut dec = Decoder::new();
            // Terminate with an extra delimiter in case the flip destroyed ours.
            w.push(0);
            for r in feed(&mut dec, &w) {
                if let Ok(frame) = r {
                    assert_eq!(
                        frame,
                        Frame { sequence: 77, message: msg },
                        "corruption at byte {i} xor {flip:#04x} produced a different accepted frame"
                    );
                    // Accepting the *original* frame is only possible if the
                    // flip was outside it — impossible here, so fail loudly.
                    panic!("corrupted wire decoded successfully (byte {i}, xor {flip:#04x})");
                }
            }
        }
    }
}

#[test]
fn truncation_detected_and_stream_recovers() {
    let msg = all_messages()[2]; // Status
    let wire = encode_to_vec(&msg, 5);
    let good = encode_to_vec(&msg, 6);
    for cut in 1..wire.len() - 1 {
        let mut dec = Decoder::new();
        let mut stream = wire[..wire.len() - 1 - cut].to_vec();
        stream.push(0); // delimiter arrives after the truncated body
        let results = feed(&mut dec, &stream);
        assert!(results.len() <= 1);
        for r in &results {
            assert!(r.is_err(), "truncated by {cut} must not decode: {r:?}");
        }
        // Recovery: next full frame decodes.
        let results = feed(&mut dec, &good);
        assert_eq!(
            results,
            vec![Ok(Frame {
                sequence: 6,
                message: msg
            })]
        );
    }
}

#[test]
fn wrong_version_rejected() {
    let raw = build_raw_frame(2, MSG_QUERY, 8, 0, &1u64.to_le_bytes());
    let mut dec = Decoder::new();
    let results = feed(&mut dec, &to_wire(&raw));
    assert_eq!(results, vec![Err(DecodeError::Version(2))]);
    let raw = build_raw_frame(0xFF, MSG_QUERY, 8, 0, &1u64.to_le_bytes());
    let results = feed(&mut dec, &to_wire(&raw));
    assert_eq!(results, vec![Err(DecodeError::Version(0xFF))]);
}

#[test]
fn unknown_message_type_rejected() {
    for t in [0x00u8, 0x0C, 0x0F, 0x1A, 0x77, 0xFF] {
        let raw = build_raw_frame(1, t, 4, 0, &[1, 2, 3, 4]);
        let mut dec = Decoder::new();
        let results = feed(&mut dec, &to_wire(&raw));
        assert_eq!(
            results,
            vec![Err(DecodeError::UnknownMessageType(t))],
            "type {t:#04x}"
        );
    }
}

#[test]
fn declared_length_mismatch_rejected() {
    // Header says 12 payload bytes but only 8 are present (valid CRC).
    let raw = build_raw_frame(1, MSG_QUERY, 12, 0, &1u64.to_le_bytes());
    let mut dec = Decoder::new();
    let results = feed(&mut dec, &to_wire(&raw));
    assert_eq!(
        results,
        vec![Err(DecodeError::LengthMismatch {
            declared: 12,
            actual: 8
        })]
    );
}

#[test]
fn wrong_payload_size_for_known_type_rejected() {
    // Consistent header/CRC, but Query payload must be exactly 8 bytes.
    let raw = build_raw_frame(1, MSG_QUERY, 12, 0, &[0u8; 12]);
    let mut dec = Decoder::new();
    let results = feed(&mut dec, &to_wire(&raw));
    assert_eq!(
        results,
        vec![Err(DecodeError::PayloadLength {
            message_type: MSG_QUERY,
            expected: 8,
            actual: 12
        })]
    );
}

#[test]
fn invalid_state_rejected() {
    for bad_state in [7u8, 42, 0xFF] {
        let mut payload = [0u8; 40];
        payload[38] = bad_state;
        let raw = build_raw_frame(1, MSG_SAMPLE, 40, 0, &payload);
        let mut dec = Decoder::new();
        let results = feed(&mut dec, &to_wire(&raw));
        assert_eq!(results, vec![Err(DecodeError::InvalidState(bad_state))]);

        let mut payload = [0u8; 28];
        payload[20] = bad_state;
        let raw = build_raw_frame(1, MSG_STATUS, 28, 0, &payload);
        let results = feed(&mut dec, &to_wire(&raw));
        assert_eq!(results, vec![Err(DecodeError::InvalidState(bad_state))]);
    }
}

#[test]
fn nonzero_reserved_bytes_ignored_on_receive() {
    // Sample.reserved = 0xAA
    let mut payload = [0u8; 40];
    payload[39] = 0xAA;
    let raw = build_raw_frame(1, MSG_SAMPLE, 40, 9, &payload);
    let mut dec = Decoder::new();
    let results = feed(&mut dec, &to_wire(&raw));
    assert_eq!(results.len(), 1);
    match results[0] {
        Ok(Frame {
            sequence: 9,
            message: Message::Sample(_),
        }) => {}
        ref other => panic!("expected sample, got {other:?}"),
    }

    // Ack.reserved = 0xBEEF
    let mut payload = [0u8; 16];
    payload[2..4].copy_from_slice(&0xBEEFu16.to_le_bytes());
    let raw = build_raw_frame(1, MSG_ACK, 16, 9, &payload);
    let results = feed(&mut dec, &to_wire(&raw));
    assert!(matches!(
        results[0],
        Ok(Frame {
            message: Message::Ack(_),
            ..
        })
    ));
}

#[test]
fn empty_and_tiny_frames_rejected_quietly() {
    let mut dec = Decoder::new();
    // Bare delimiters are idle, not errors.
    assert_eq!(feed(&mut dec, &[0, 0, 0, 0]), vec![]);
    // A one-byte body is COBS-decodable to zero bytes -> too short.
    let results = feed(&mut dec, &[0x01, 0x00]);
    assert_eq!(results, vec![Err(DecodeError::TooShort { len: 0 })]);
    // A few nonzero bytes -> decoded frame shorter than 12.
    let results = feed(&mut dec, &[0x03, 0x11, 0x22, 0x00]);
    assert_eq!(results, vec![Err(DecodeError::TooShort { len: 2 })]);
}

#[test]
fn overlong_input_rejected_and_recovers() {
    let mut dec = Decoder::new();
    // 1000 nonzero bytes, then delimiter.
    let mut stream = vec![0x42u8; 1000];
    stream.push(0);
    let results = feed(&mut dec, &stream);
    assert_eq!(results, vec![Err(DecodeError::TooLong)]);
    // Exactly at the stuffed-size limit but decoding past MAX_FRAME_LEN.
    let mut stream = vec![0x42u8; MAX_ENCODED_LEN - 1];
    stream[0] = 0xFF; // plausible COBS group code
    stream.push(0);
    let results = feed(&mut dec, &stream);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_err());
    // Recovery.
    let msg = Message::Query(Query { command_nonce: 1 });
    let results = feed(&mut dec, &encode_to_vec(&msg, 1));
    assert_eq!(
        results,
        vec![Ok(Frame {
            sequence: 1,
            message: msg
        })]
    );
}

/// Deterministic xorshift PRNG so the "fuzz" tests are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 32) as u8
    }
}

#[test]
fn random_noise_never_panics_never_decodes_by_luck() {
    let mut rng = Rng(0x5EED_5EED_5EED_5EED);
    let mut dec = Decoder::new();
    let mut accepted = 0u32;
    for _ in 0..200_000 {
        if let Some(Ok(_)) = dec.push(rng.byte()) {
            accepted += 1;
        }
    }
    // A random 12+ byte frame passing structural checks AND CRC-32 is a
    // ~2^-32 event per delimiter; zero is the only sane outcome here.
    assert_eq!(accepted, 0, "random noise must not produce valid frames");
}

#[test]
fn frames_interleaved_with_noise_all_recovered() {
    let mut rng = Rng(0xBADC_0FFE_E0DD_F00D);
    let msgs = all_messages();
    let mut stream = Vec::new();
    let mut expected = Vec::new();
    for round in 0..50u32 {
        // Noise burst (may contain zeros -> extra error/idle delimiters).
        for _ in 0..(rng.next() % 40) {
            stream.push(rng.byte());
        }
        // Delimiter guarantees the next frame starts clean.
        stream.push(0);
        let msg = msgs[(rng.next() % msgs.len() as u64) as usize];
        stream.extend_from_slice(&encode_to_vec(&msg, round));
        expected.push(Frame {
            sequence: round,
            message: msg,
        });
    }
    let mut dec = Decoder::new();
    let decoded: Vec<Frame> = stream
        .iter()
        .filter_map(|&b| dec.push(b))
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(
        decoded, expected,
        "every injected frame must decode exactly once"
    );
}

#[test]
fn decoder_reset_discards_partial_frame() {
    let msg = Message::Query(Query { command_nonce: 5 });
    let wire = encode_to_vec(&msg, 3);
    let mut dec = Decoder::new();
    // Feed half a frame, then reset (models a reconnect).
    for &b in &wire[..wire.len() / 2] {
        assert!(dec.push(b).is_none());
    }
    dec.reset();
    // A full frame now decodes cleanly with no error in between.
    let results = feed(&mut dec, &wire);
    assert_eq!(
        results,
        vec![Ok(Frame {
            sequence: 3,
            message: msg
        })]
    );
}

#[test]
fn encode_rejects_small_buffer() {
    let msg = Message::Sample(Sample {
        monotonic_us: 0,
        rtc_unix_ms: 0,
        run_id: 0,
        flags: 0,
        excitation_raw: 0,
        thermistor_raw: [0; 6],
        v24_raw: 0,
        state: BoardState::SafeIdle,
    });
    let mut tiny = [0u8; 10];
    assert_eq!(encode(&msg, 0, &mut tiny), Err(EncodeError::BufferTooSmall));
}

#[test]
fn sequence_wraparound_encodes() {
    let msg = Message::Query(Query { command_nonce: 0 });
    for seq in [0u32, 1, u32::MAX - 1, u32::MAX] {
        let wire = encode_to_vec(&msg, seq);
        let mut dec = Decoder::new();
        let results = feed(&mut dec, &wire);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].unwrap().sequence, seq);
    }
}
