//! UDP datagram codec for the sap-flux gateway (nRF9160 Feather) ↔ server
//! link, as specified in `docs/field-protocol.md` section 2.
//!
//! `no_std`, no allocation: the same code is meant to run on the nRF9160
//! gateway and on the server. Enable the `std` feature to get
//! `std::error::Error` on [`Error`].
//!
//! # Datagram layout
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 2 | magic `b"SF"` |
//! | 2 | 1 | version = 1 |
//! | 3 | 1 | type |
//! | 4 | 8 | `device_id: u64` |
//! | 12 | 4 | `boot_id: u32` |
//! | 16 | 4 | `seq: u32` |
//! | 20 | n | body |
//! | 20+n | 8 | HMAC-SHA256(key, bytes `0..20+n`), first 8 bytes |
//!
//! All integers are little-endian. Datagrams are at most
//! [`MAX_DATAGRAM_LEN`] bytes.
//!
//! Receivers call [`peek_header`] to find the device key, then [`open`] to
//! verify the MAC and parse the body. Senders call [`seal`]. Replay
//! protection (`seq` per `boot_id`) is the receiver's job; this crate only
//! parses.

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(unsafe_code)]
#![warn(missing_docs)]

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub use sap_flux_protocol::crc::crc32;

/// Datagram magic.
pub const MAGIC: [u8; 2] = *b"SF";
/// Datagram format version.
pub const VERSION: u8 = 1;
/// Fixed header size.
pub const HEADER_LEN: usize = 20;
/// Truncated MAC size.
pub const MAC_LEN: usize = 8;
/// Largest datagram either side may send.
pub const MAX_DATAGRAM_LEN: usize = 1200;
/// Largest body that fits in [`MAX_DATAGRAM_LEN`].
pub const MAX_BODY_LEN: usize = MAX_DATAGRAM_LEN - HEADER_LEN - MAC_LEN;
/// Largest `data` in one [`WindowPart`].
pub const MAX_PART_DATA: usize = 1100;
/// Largest number of parts per window (the ack bitmap has 256 bits).
pub const MAX_PARTS: u16 = 256;
/// Largest event text in bytes.
pub const MAX_EVENT_TEXT: usize = 256;

/// `0x01` CheckIn, gateway → server.
pub const TYPE_CHECKIN: u8 = 0x01;
/// `0x02` WindowPart, gateway → server.
pub const TYPE_WINDOW_PART: u8 = 0x02;
/// `0x03` Event, gateway → server.
pub const TYPE_EVENT: u8 = 0x03;
/// `0x81` CheckInReply, server → gateway.
pub const TYPE_CHECKIN_REPLY: u8 = 0x81;
/// `0x82` WindowAck, server → gateway.
pub const TYPE_WINDOW_ACK: u8 = 0x82;
/// `0x83` EventAck, server → gateway.
pub const TYPE_EVENT_ACK: u8 = 0x83;

/// CheckIn flag: gateway clock valid.
pub const CHECKIN_FLAG_TIME_VALID: u16 = 1 << 0;
/// CheckIn flag: board link OK.
pub const CHECKIN_FLAG_BOARD_LINK_OK: u16 = 1 << 1;

/// CheckInReply request flag: send pending windows.
pub const REQUEST_SEND_WINDOWS: u16 = 1 << 0;
/// CheckInReply request flag: reboot the gateway.
pub const REQUEST_REBOOT: u16 = 1 << 1;

/// Event code: gateway boot.
pub const EVENT_BOOT: u16 = 1;
/// Event code: board link lost.
pub const EVENT_BOARD_LINK_LOST: u16 = 2;
/// Event code: board link restored.
pub const EVENT_BOARD_LINK_RESTORED: u16 = 3;
/// Event code: RTC step applied (text has old/new).
pub const EVENT_RTC_STEP: u16 = 4;
/// Event code: modem error.
pub const EVENT_MODEM_ERROR: u16 = 5;
/// Event code: window dropped.
pub const EVENT_WINDOW_DROPPED: u16 = 6;

/// Schedule flag bit 0: only pulse while `now < lease_until`.
pub const SCHEDULE_FLAG_REQUIRE_LEASE: u8 = 1 << 0;
/// Board-side mandatory cooldown after a pulse, seconds
/// (`firmware/sap-flux-core/src/pulse.rs` `COOLDOWN_MS`, production build).
pub const BOARD_COOLDOWN_S: u32 = 600;

/// Codec errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Shorter than header + MAC, or a body shorter than its fixed part.
    TooShort,
    /// Longer than [`MAX_DATAGRAM_LEN`] or a field over its limit.
    TooLong,
    /// Magic is not `b"SF"`.
    BadMagic,
    /// Unsupported version byte.
    BadVersion(u8),
    /// Unknown datagram type.
    UnknownType(u8),
    /// Body length disagrees with its type or its own length fields.
    BadLength,
    /// MAC check failed.
    BadMac,
    /// Output buffer too small.
    BufferTooSmall,
    /// A field value violates the spec (e.g. `part >= part_count`).
    InvalidField,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::TooShort => f.write_str("datagram too short"),
            Error::TooLong => f.write_str("datagram or field too long"),
            Error::BadMagic => f.write_str("bad magic"),
            Error::BadVersion(v) => write!(f, "unsupported version {v}"),
            Error::UnknownType(t) => write!(f, "unknown type 0x{t:02x}"),
            Error::BadLength => f.write_str("body length mismatch"),
            Error::BadMac => f.write_str("bad MAC"),
            Error::BufferTooSmall => f.write_str("output buffer too small"),
            Error::InvalidField => f.write_str("invalid field value"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// Datagram header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Datagram type (`TYPE_*`).
    pub msg_type: u8,
    /// Gateway id (from the IMEI).
    pub device_id: u64,
    /// Random per gateway boot.
    pub boot_id: u32,
    /// Per-boot sequence number, starting at 1.
    pub seq: u32,
}

impl Header {
    /// Header for the reply to `self`: same device, boot and seq, type
    /// `reply_type`.
    pub fn reply(&self, reply_type: u8) -> Header {
        Header {
            msg_type: reply_type,
            ..*self
        }
    }
}

/// Pulse schedule: the 20 `SetSchedule` bytes `enabled`..`bin_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    /// Pulsing enabled.
    pub enabled: bool,
    /// `SCHEDULE_FLAG_*` bits.
    pub flags: u8,
    /// Heater on time: 500, 1000 or 1500 ms.
    pub pulse_duration_ms: u16,
    /// Pulse period, ≥ 600 s.
    pub interval_s: u32,
    /// Pulses start where `(t - phase_s) mod interval_s == 0`.
    pub phase_s: u32,
    /// Pre-pulse baseline, 10–300 s.
    pub baseline_s: u16,
    /// Post-pulse recovery, 60–600 s.
    pub recovery_s: u16,
    /// Frame rate: 10, 20 or 25 Hz.
    pub sample_hz: u16,
    /// Uplink bin width: 250, 500 or 1000 ms.
    pub bin_ms: u16,
}

/// Size of [`Schedule`] on the wire.
pub const SCHEDULE_LEN: usize = 20;

/// Why a [`Schedule`] is invalid (mirrors the board's `INVALID_PARAMETER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// `pulse_duration_ms` not in {500, 1000, 1500}.
    PulseDuration,
    /// `interval_s` < 600.
    Interval,
    /// `baseline_s` outside 10–300.
    Baseline,
    /// `recovery_s` outside 60–600.
    Recovery,
    /// `baseline_s + recovery_s + cooldown >= interval_s`.
    TooTight,
    /// `sample_hz` not in {10, 20, 25}.
    SampleHz,
    /// `bin_ms` not in {250, 500, 1000}.
    BinMs,
    /// Undefined `flags` bits set.
    Flags,
}

impl core::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            ScheduleError::PulseDuration => "pulse_duration_ms must be 500, 1000 or 1500",
            ScheduleError::Interval => "interval_s must be >= 600",
            ScheduleError::Baseline => "baseline_s must be 10..=300",
            ScheduleError::Recovery => "recovery_s must be 60..=600",
            ScheduleError::TooTight => {
                "baseline_s + recovery_s + 600 s cooldown must be < interval_s"
            }
            ScheduleError::SampleHz => "sample_hz must be 10, 20 or 25",
            ScheduleError::BinMs => "bin_ms must be 250, 500 or 1000",
            ScheduleError::Flags => "only flags bit 0 (REQUIRE_LEASE) is defined",
        })
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ScheduleError {}

impl Schedule {
    /// Factory default of a blank board EEPROM (disabled).
    pub const FACTORY_DEFAULT: Schedule = Schedule {
        enabled: false,
        flags: 0,
        pulse_duration_ms: 1000,
        interval_s: 3600,
        phase_s: 0,
        baseline_s: 60,
        recovery_s: 180,
        sample_hz: 25,
        bin_ms: 500,
    };

    /// Check the board's `SetSchedule` limits.
    pub fn validate(&self) -> Result<(), ScheduleError> {
        if !matches!(self.pulse_duration_ms, 500 | 1000 | 1500) {
            return Err(ScheduleError::PulseDuration);
        }
        if self.interval_s < 600 {
            return Err(ScheduleError::Interval);
        }
        if !(10..=300).contains(&self.baseline_s) {
            return Err(ScheduleError::Baseline);
        }
        if !(60..=600).contains(&self.recovery_s) {
            return Err(ScheduleError::Recovery);
        }
        let busy = self.baseline_s as u64 + self.recovery_s as u64 + BOARD_COOLDOWN_S as u64;
        if busy >= self.interval_s as u64 {
            return Err(ScheduleError::TooTight);
        }
        if !matches!(self.sample_hz, 10 | 20 | 25) {
            return Err(ScheduleError::SampleHz);
        }
        if !matches!(self.bin_ms, 250 | 500 | 1000) {
            return Err(ScheduleError::BinMs);
        }
        if self.flags & !SCHEDULE_FLAG_REQUIRE_LEASE != 0 {
            return Err(ScheduleError::Flags);
        }
        Ok(())
    }

    /// Wire bytes.
    pub fn to_bytes(&self) -> [u8; SCHEDULE_LEN] {
        let mut b = [0u8; SCHEDULE_LEN];
        let mut w = W { buf: &mut b, pos: 0 };
        w.u8(self.enabled as u8);
        w.u8(self.flags);
        w.u16(self.pulse_duration_ms);
        w.u32(self.interval_s);
        w.u32(self.phase_s);
        w.u16(self.baseline_s);
        w.u16(self.recovery_s);
        w.u16(self.sample_hz);
        w.u16(self.bin_ms);
        b
    }

    /// Parse wire bytes (no validation; `enabled` is any nonzero byte).
    pub fn from_bytes(b: &[u8; SCHEDULE_LEN]) -> Schedule {
        let mut r = R { buf: b, pos: 0 };
        Schedule {
            enabled: r.u8() != 0,
            flags: r.u8(),
            pulse_duration_ms: r.u16(),
            interval_s: r.u32(),
            phase_s: r.u32(),
            baseline_s: r.u16(),
            recovery_s: r.u16(),
            sample_hz: r.u16(),
            bin_ms: r.u16(),
        }
    }

    /// `FieldStatus.schedule_crc` the board reports once it has accepted
    /// this schedule: low 16 bits of CRC-32 over the 20 bytes.
    pub fn crc16(&self) -> u16 {
        (crc32(&self.to_bytes()) & 0xFFFF) as u16
    }
}

/// `0x01` CheckIn body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckIn<'a> {
    /// Gateway clock at send.
    pub t1_unix_ms: i64,
    /// Gateway firmware version.
    pub gw_fw: u32,
    /// Battery voltage.
    pub batt_mv: u16,
    /// LTE RSRP.
    pub rsrp_dbm: i16,
    /// Gateway uptime.
    pub uptime_s: u32,
    /// `CHECKIN_FLAG_*` bits.
    pub flags: u16,
    /// The board's latest Hello, Status and FieldStatus as decoded v1
    /// frames (header + payload + CRC, no COBS), concatenated.
    pub board_frames: &'a [u8],
}

/// `0x81` CheckInReply body (60 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckInReply {
    /// Echo of `CheckIn.t1_unix_ms`.
    pub t1_unix_ms: i64,
    /// Server receive time.
    pub t2_unix_ms: i64,
    /// Server send time.
    pub t3_unix_ms: i64,
    /// Server config version for this device.
    pub config_version: u32,
    /// Schedule the board should run.
    pub schedule: Schedule,
    /// Lease length the gateway should grant, from now.
    pub lease_s: u32,
    /// Gateway check-in period.
    pub wake_interval_s: u32,
    /// `REQUEST_*` bits.
    pub request_flags: u16,
}

impl CheckInReply {
    /// NTP-style offset and round trip from the four timestamps, with `t4`
    /// the gateway's receive time: `(offset_ms, rtt_ms)`. The offset is
    /// what to add to the gateway clock to get server time.
    pub fn offset_rtt(&self, t4_unix_ms: i64) -> (i64, i64) {
        let t1 = self.t1_unix_ms;
        let t2 = self.t2_unix_ms;
        let t3 = self.t3_unix_ms;
        let offset = ((t2 - t1) + (t3 - t4_unix_ms)) / 2;
        let rtt = (t4_unix_ms - t1) - (t3 - t2);
        (offset, rtt)
    }
}

/// `0x02` WindowPart body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPart<'a> {
    /// Board id from the board's Hello.
    pub board_id: u64,
    /// Window run id.
    pub run_id: u32,
    /// Part index, `< part_count`.
    pub part: u16,
    /// Number of parts, 1..=256.
    pub part_count: u16,
    /// Total reassembled length (WindowInfo frame + data).
    pub total_len: u32,
    /// Part bytes, ≤ 1100.
    pub data: &'a [u8],
}

/// `0x82` WindowAck body (48 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowAck {
    /// Board id.
    pub board_id: u64,
    /// Run id.
    pub run_id: u32,
    /// The whole window is stored and verified.
    pub complete: bool,
    /// Part count the server is tracking.
    pub part_count: u16,
    /// Bit `i` (byte `i / 8`, bit `i % 8`, LSB first) set = part `i` stored.
    pub received_bitmap: [u8; 32],
}

impl WindowAck {
    /// Whether part `i` is marked stored.
    pub fn has_part(&self, i: u16) -> bool {
        let i = i as usize;
        i < 256 && self.received_bitmap[i / 8] & (1 << (i % 8)) != 0
    }

    /// Mark part `i` stored.
    pub fn set_part(&mut self, i: u16) {
        let i = i as usize;
        if i < 256 {
            self.received_bitmap[i / 8] |= 1 << (i % 8);
        }
    }

    /// Iterator over parts `0..part_count` not yet stored.
    pub fn missing(&self) -> impl Iterator<Item = u16> + '_ {
        (0..self.part_count).filter(|&i| !self.has_part(i))
    }
}

/// `0x03` Event body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event<'a> {
    /// Gateway time of the event.
    pub t_unix_ms: i64,
    /// `EVENT_*` code.
    pub code: u16,
    /// UTF-8 text, ≤ 256 bytes (not validated here).
    pub text: &'a [u8],
}

/// `0x83` EventAck body (8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventAck {
    /// Echo of `Event.t_unix_ms`.
    pub t_unix_ms: i64,
}

/// Any datagram body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body<'a> {
    /// `0x01`
    CheckIn(CheckIn<'a>),
    /// `0x02`
    WindowPart(WindowPart<'a>),
    /// `0x03`
    Event(Event<'a>),
    /// `0x81`
    CheckInReply(CheckInReply),
    /// `0x82`
    WindowAck(WindowAck),
    /// `0x83`
    EventAck(EventAck),
}

impl Body<'_> {
    /// Wire type byte.
    pub fn msg_type(&self) -> u8 {
        match self {
            Body::CheckIn(_) => TYPE_CHECKIN,
            Body::WindowPart(_) => TYPE_WINDOW_PART,
            Body::Event(_) => TYPE_EVENT,
            Body::CheckInReply(_) => TYPE_CHECKIN_REPLY,
            Body::WindowAck(_) => TYPE_WINDOW_ACK,
            Body::EventAck(_) => TYPE_EVENT_ACK,
        }
    }

    fn encoded_len(&self) -> usize {
        match self {
            Body::CheckIn(b) => 24 + b.board_frames.len(),
            Body::WindowPart(b) => 22 + b.data.len(),
            Body::Event(b) => 12 + b.text.len(),
            Body::CheckInReply(_) => 60,
            Body::WindowAck(_) => 48,
            Body::EventAck(_) => 8,
        }
    }

    fn check_limits(&self) -> Result<(), Error> {
        match self {
            Body::WindowPart(b) => {
                if b.data.len() > MAX_PART_DATA {
                    return Err(Error::TooLong);
                }
                if b.part_count == 0 || b.part_count > MAX_PARTS || b.part >= b.part_count {
                    return Err(Error::InvalidField);
                }
            }
            Body::Event(b) if b.text.len() > MAX_EVENT_TEXT => return Err(Error::TooLong),
            Body::CheckIn(b) if b.board_frames.len() > u16::MAX as usize => {
                return Err(Error::TooLong)
            }
            Body::WindowAck(b) if b.part_count > MAX_PARTS => return Err(Error::InvalidField),
            _ => {}
        }
        Ok(())
    }

    fn write(&self, w: &mut W<'_>) {
        match self {
            Body::CheckIn(b) => {
                w.i64(b.t1_unix_ms);
                w.u32(b.gw_fw);
                w.u16(b.batt_mv);
                w.i16(b.rsrp_dbm);
                w.u32(b.uptime_s);
                w.u16(b.flags);
                w.u16(b.board_frames.len() as u16);
                w.put(b.board_frames);
            }
            Body::WindowPart(b) => {
                w.u64(b.board_id);
                w.u32(b.run_id);
                w.u16(b.part);
                w.u16(b.part_count);
                w.u32(b.total_len);
                w.u16(b.data.len() as u16);
                w.put(b.data);
            }
            Body::Event(b) => {
                w.i64(b.t_unix_ms);
                w.u16(b.code);
                w.u16(b.text.len() as u16);
                w.put(b.text);
            }
            Body::CheckInReply(b) => {
                w.i64(b.t1_unix_ms);
                w.i64(b.t2_unix_ms);
                w.i64(b.t3_unix_ms);
                w.u32(b.config_version);
                w.put(&b.schedule.to_bytes());
                w.u32(b.lease_s);
                w.u32(b.wake_interval_s);
                w.u16(b.request_flags);
                w.u16(0);
            }
            Body::WindowAck(b) => {
                w.u64(b.board_id);
                w.u32(b.run_id);
                w.u8(b.complete as u8);
                w.u8(0);
                w.u16(b.part_count);
                w.put(&b.received_bitmap);
            }
            Body::EventAck(b) => w.i64(b.t_unix_ms),
        }
    }
}

fn parse_body(msg_type: u8, body: &[u8]) -> Result<Body<'_>, Error> {
    let mut r = R { buf: body, pos: 0 };
    let need = |n: usize| if body.len() < n { Err(Error::TooShort) } else { Ok(()) };
    let exact = |n: usize| if body.len() != n { Err(Error::BadLength) } else { Ok(()) };
    let parsed = match msg_type {
        TYPE_CHECKIN => {
            need(24)?;
            let t1_unix_ms = r.i64();
            let gw_fw = r.u32();
            let batt_mv = r.u16();
            let rsrp_dbm = r.i16();
            let uptime_s = r.u32();
            let flags = r.u16();
            let n = r.u16() as usize;
            exact(24 + n)?;
            Body::CheckIn(CheckIn {
                t1_unix_ms,
                gw_fw,
                batt_mv,
                rsrp_dbm,
                uptime_s,
                flags,
                board_frames: r.take(n),
            })
        }
        TYPE_WINDOW_PART => {
            need(22)?;
            let board_id = r.u64();
            let run_id = r.u32();
            let part = r.u16();
            let part_count = r.u16();
            let total_len = r.u32();
            let n = r.u16() as usize;
            exact(22 + n)?;
            Body::WindowPart(WindowPart {
                board_id,
                run_id,
                part,
                part_count,
                total_len,
                data: r.take(n),
            })
        }
        TYPE_EVENT => {
            need(12)?;
            let t_unix_ms = r.i64();
            let code = r.u16();
            let n = r.u16() as usize;
            exact(12 + n)?;
            Body::Event(Event {
                t_unix_ms,
                code,
                text: r.take(n),
            })
        }
        TYPE_CHECKIN_REPLY => {
            exact(60)?;
            let t1_unix_ms = r.i64();
            let t2_unix_ms = r.i64();
            let t3_unix_ms = r.i64();
            let config_version = r.u32();
            let mut s = [0u8; SCHEDULE_LEN];
            s.copy_from_slice(r.take(SCHEDULE_LEN));
            let lease_s = r.u32();
            let wake_interval_s = r.u32();
            let request_flags = r.u16();
            let _reserved = r.u16();
            Body::CheckInReply(CheckInReply {
                t1_unix_ms,
                t2_unix_ms,
                t3_unix_ms,
                config_version,
                schedule: Schedule::from_bytes(&s),
                lease_s,
                wake_interval_s,
                request_flags,
            })
        }
        TYPE_WINDOW_ACK => {
            exact(48)?;
            let board_id = r.u64();
            let run_id = r.u32();
            let complete = r.u8() != 0;
            let _reserved = r.u8();
            let part_count = r.u16();
            let mut received_bitmap = [0u8; 32];
            received_bitmap.copy_from_slice(r.take(32));
            Body::WindowAck(WindowAck {
                board_id,
                run_id,
                complete,
                part_count,
                received_bitmap,
            })
        }
        TYPE_EVENT_ACK => {
            exact(8)?;
            Body::EventAck(EventAck {
                t_unix_ms: r.i64(),
            })
        }
        other => return Err(Error::UnknownType(other)),
    };
    parsed.check_limits()?;
    Ok(parsed)
}

/// Compute the truncated MAC over `data`.
pub fn mac(key: &[u8], data: &[u8]) -> [u8; MAC_LEN] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(data);
    let full = m.finalize().into_bytes();
    let mut out = [0u8; MAC_LEN];
    out.copy_from_slice(&full[..MAC_LEN]);
    out
}

/// Parse the header without checking the MAC (to look up the key).
pub fn peek_header(datagram: &[u8]) -> Result<Header, Error> {
    if datagram.len() > MAX_DATAGRAM_LEN {
        return Err(Error::TooLong);
    }
    if datagram.len() < HEADER_LEN + MAC_LEN {
        return Err(Error::TooShort);
    }
    if datagram[0..2] != MAGIC {
        return Err(Error::BadMagic);
    }
    if datagram[2] != VERSION {
        return Err(Error::BadVersion(datagram[2]));
    }
    let mut r = R {
        buf: &datagram[3..HEADER_LEN],
        pos: 0,
    };
    Ok(Header {
        msg_type: r.u8(),
        device_id: r.u64(),
        boot_id: r.u32(),
        seq: r.u32(),
    })
}

/// Verify the MAC with `key` and parse the datagram.
pub fn open<'a>(datagram: &'a [u8], key: &[u8]) -> Result<(Header, Body<'a>), Error> {
    let header = peek_header(datagram)?;
    let mac_at = datagram.len() - MAC_LEN;
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(&datagram[..mac_at]);
    m.verify_truncated_left(&datagram[mac_at..])
        .map_err(|_| Error::BadMac)?;
    let body = parse_body(header.msg_type, &datagram[HEADER_LEN..mac_at])?;
    Ok((header, body))
}

/// Encode and MAC a datagram into `out`, returning its length. The
/// header's `msg_type` is taken from `body`.
pub fn seal(header: &Header, body: &Body<'_>, key: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    body.check_limits()?;
    let total = HEADER_LEN + body.encoded_len() + MAC_LEN;
    if total > MAX_DATAGRAM_LEN {
        return Err(Error::TooLong);
    }
    if out.len() < total {
        return Err(Error::BufferTooSmall);
    }
    let mut w = W { buf: out, pos: 0 };
    w.put(&MAGIC);
    w.u8(VERSION);
    w.u8(body.msg_type());
    w.u64(header.device_id);
    w.u32(header.boot_id);
    w.u32(header.seq);
    body.write(&mut w);
    let mac_at = w.pos;
    debug_assert_eq!(mac_at + MAC_LEN, total);
    let tag = mac(key, &out[..mac_at]);
    out[mac_at..total].copy_from_slice(&tag);
    Ok(total)
}

struct R<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        s
    }
    fn arr<const N: usize>(&mut self) -> [u8; N] {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N));
        a
    }
    fn u8(&mut self) -> u8 {
        self.arr::<1>()[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.arr())
    }
    fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.arr())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.arr())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.arr())
    }
    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.arr())
    }
}

struct W<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl W<'_> {
    fn put(&mut self, b: &[u8]) {
        self.buf[self.pos..self.pos + b.len()].copy_from_slice(b);
        self.pos += b.len();
    }
    fn u8(&mut self, v: u8) {
        self.put(&[v]);
    }
    fn u16(&mut self, v: u16) {
        self.put(&v.to_le_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.put(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.put(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.put(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.put(&v.to_le_bytes());
    }
}

#[cfg(test)]
mod tests;
