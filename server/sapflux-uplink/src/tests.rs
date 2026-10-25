extern crate std;
use super::*;
use std::vec::Vec;

const KEY: [u8; 32] = {
    let mut k = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        k[i] = i as u8;
        i += 1;
    }
    k
};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hdr(msg_type: u8) -> Header {
    Header {
        msg_type,
        device_id: 352656100123456,
        boot_id: 0xDEAD_BEEF,
        seq: 7,
    }
}

fn default_sched() -> Schedule {
    Schedule {
        enabled: 1,
        ..Schedule::FACTORY_DEFAULT
    }
}

// Golden bytes computed independently with Python's hmac/hashlib/struct.
const GOLDEN_EVENT: &str = "5346010340639325bd400100efbeadde07000000ff5d5b36a001000001000400626f6f74f46eb52d86da00d4";
const GOLDEN_REPLY: &str = "5346018140639325bd400100efbeadde07000000e803000000000000dc05000000000000dd05000000000000030000000100e803100e0000000000003c00b4001900f4010000302a0000100e000001000000e75c8ce48a50a136";

#[test]
fn golden_event() {
    let body = Body::Event(Event {
        t_unix_ms: 1787618352639,
        code: EVENT_BOOT,
        text: b"boot",
    });
    let mut out = [0u8; MAX_DATAGRAM_LEN];
    let n = seal(&hdr(TYPE_EVENT), &body, &KEY, &mut out).unwrap();
    assert_eq!(&out[..n], &hex(GOLDEN_EVENT)[..]);
    let (h, b) = open(&out[..n], &KEY).unwrap();
    assert_eq!(h, hdr(TYPE_EVENT));
    assert_eq!(b, body);
}

#[test]
fn golden_checkin_reply() {
    let body = Body::CheckInReply(CheckInReply {
        t1_unix_ms: 1000,
        t2_unix_ms: 1500,
        t3_unix_ms: 1501,
        config_version: 3,
        schedule: default_sched(),
        lease_s: 10800,
        wake_interval_s: 3600,
        request_flags: REQUEST_SEND_WINDOWS,
    });
    let mut out = [0u8; MAX_DATAGRAM_LEN];
    let n = seal(&hdr(TYPE_CHECKIN_REPLY), &body, &KEY, &mut out).unwrap();
    assert_eq!(n, 90);
    assert_eq!(&out[..n], &hex(GOLDEN_REPLY)[..]);
}

#[test]
fn schedule_bytes_and_crc() {
    let s = default_sched();
    assert_eq!(&s.to_bytes()[..], &hex("0100e803100e0000000000003c00b4001900f4010000")[..]);
    assert_eq!(s.crc16(), 0x93fb);
    assert_eq!(Schedule::from_bytes(&s.to_bytes()), s);
}

#[test]
fn schedule_limits() {
    let ok = default_sched();
    assert!(validate_schedule(&ok).is_ok());
    assert!(validate_schedule(&Schedule::FACTORY_DEFAULT).is_ok());
    let bad = |f: fn(&mut Schedule), e: ScheduleError| {
        let mut s = ok;
        f(&mut s);
        assert_eq!(validate_schedule(&s), Err(e));
    };
    bad(|s| s.pulse_duration_ms = 750, ScheduleError::PulseDuration);
    bad(|s| s.interval_s = 599, ScheduleError::Interval);
    bad(|s| s.baseline_s = 9, ScheduleError::Baseline);
    bad(|s| s.baseline_s = 301, ScheduleError::Baseline);
    bad(|s| s.recovery_s = 59, ScheduleError::Recovery);
    bad(|s| s.recovery_s = 601, ScheduleError::Recovery);
    bad(|s| s.sample_hz = 50, ScheduleError::SampleHz);
    bad(|s| s.bin_ms = 200, ScheduleError::BinMs);
    bad(|s| s.flags = 1 << 6, ScheduleError::Flags);
    bad(|s| s.sfk1_presets = [0b11_00, 0], ScheduleError::Sfk1Preset);
    // 60 + 180 + 600 = 840: interval 840 is too tight, 841 passes.
    bad(|s| s.interval_s = 840, ScheduleError::Timing);
    bad(|s| s.enabled = 2, ScheduleError::Enabled);
    let mut s = ok;
    s.interval_s = 841;
    assert!(validate_schedule(&s).is_ok());
}

#[test]
fn roundtrip_all_types() {
    let frames = [1u8, 2, 3, 4, 5];
    let data = [0xAAu8; MAX_PART_DATA];
    let mut ack = WindowAck {
        board_id: 0xD15C_55B4_116E_105E,
        run_id: 9,
        complete: false,
        part_count: 12,
        received_bitmap: [0; 32],
    };
    ack.set_part(0);
    ack.set_part(9);
    let bodies = [
        Body::CheckIn(CheckIn {
            t1_unix_ms: -5,
            gw_fw: 0x0001_0203,
            batt_mv: 3300,
            rsrp_dbm: -101,
            uptime_s: 42,
            flags: CHECKIN_FLAG_TIME_VALID | CHECKIN_FLAG_BOARD_LINK_OK,
            board_frames: &frames,
        }),
        Body::WindowPart(WindowPart {
            board_id: 0xD15C_55B4_116E_105E,
            run_id: 9,
            part: 11,
            part_count: 12,
            total_len: 12634,
            data: &data,
        }),
        Body::Event(Event {
            t_unix_ms: 1,
            code: EVENT_RTC_STEP,
            text: "old=1 new=2".as_bytes(),
        }),
        Body::WindowAck(ack),
        Body::EventAck(EventAck { t_unix_ms: 77 }),
    ];
    for body in bodies {
        let mut out = [0u8; MAX_DATAGRAM_LEN];
        let n = seal(&hdr(body.msg_type()), &body, &KEY, &mut out).unwrap();
        assert!(n <= MAX_DATAGRAM_LEN);
        let (h, b) = open(&out[..n], &KEY).unwrap();
        assert_eq!(h.msg_type, body.msg_type());
        assert_eq!(b, body);
    }
    let missing: Vec<u16> = ack.missing().collect();
    assert_eq!(missing, [1, 2, 3, 4, 5, 6, 7, 8, 10, 11]);
    assert_eq!(ack.received_bitmap[1], 0b10);
}

#[test]
fn max_window_part_fits() {
    // 20 + 22 + 1100 + 8 = 1150 <= 1200
    let data = [0u8; MAX_PART_DATA];
    let body = Body::WindowPart(WindowPart {
        board_id: 1,
        run_id: 1,
        part: 0,
        part_count: 1,
        total_len: 1100,
        data: &data,
    });
    let mut out = [0u8; MAX_DATAGRAM_LEN];
    assert_eq!(seal(&hdr(TYPE_WINDOW_PART), &body, &KEY, &mut out), Ok(1150));
}

#[test]
fn rejects_tampering_and_garbage() {
    let body = Body::EventAck(EventAck { t_unix_ms: 77 });
    let mut out = [0u8; MAX_DATAGRAM_LEN];
    let n = seal(&hdr(TYPE_EVENT_ACK), &body, &KEY, &mut out).unwrap();
    let good = out[..n].to_vec();
    for i in 0..n {
        let mut d = good.clone();
        d[i] ^= 0x01;
        assert!(open(&d, &KEY).is_err(), "flip at byte {i} accepted");
    }
    let mut wrong_key = KEY;
    wrong_key[0] ^= 1;
    assert_eq!(open(&good, &wrong_key), Err(Error::BadMac));
    assert_eq!(open(&good[..n - 1], &KEY), Err(Error::BadMac));
    assert_eq!(open(&good[..10], &KEY), Err(Error::TooShort));
    assert_eq!(open(&[0u8; 1201], &KEY), Err(Error::TooLong));
    // Truncate the body but re-MAC: length check, not MAC, must reject.
    let mut short = good[..HEADER_LEN + 4].to_vec();
    let tag = mac(&KEY, &short);
    short.extend_from_slice(&tag);
    assert_eq!(open(&short, &KEY), Err(Error::BadLength));
}

#[test]
fn rejects_bad_part_fields() {
    let data = [0u8; 4];
    for (part, count) in [(1u16, 1u16), (0, 0), (0, 257)] {
        let body = Body::WindowPart(WindowPart {
            board_id: 1,
            run_id: 1,
            part,
            part_count: count,
            total_len: 4,
            data: &data,
        });
        let mut out = [0u8; MAX_DATAGRAM_LEN];
        assert_eq!(
            seal(&hdr(TYPE_WINDOW_PART), &body, &KEY, &mut out),
            Err(Error::InvalidField)
        );
    }
}

#[test]
fn offset_rtt() {
    // Gateway 1000 ms behind the server, 200 ms each way, 10 ms server time.
    let r = CheckInReply {
        t1_unix_ms: 10_000,
        t2_unix_ms: 11_200,
        t3_unix_ms: 11_210,
        config_version: 0,
        schedule: default_sched(),
        lease_s: 0,
        wake_interval_s: 0,
        request_flags: 0,
    };
    assert_eq!(r.offset_rtt(10_410), (1000, 400));
}
