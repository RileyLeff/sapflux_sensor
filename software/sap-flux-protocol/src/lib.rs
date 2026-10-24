//! Wire protocol for the sap-flux instrument, version 1.
//!
//! This crate is `no_std` (std is only enabled for the crate's own tests) and
//! dependency-free so the exact same encode/decode code runs on the STM32
//! (Embassy firmware) and on the host monitor.
//!
//! # Wire format
//!
//! Every message is a *frame*, byte-stuffed with [COBS](cobs) and terminated
//! by a single `0x00` delimiter byte on the wire. The decoded (unstuffed)
//! frame layout is:
//!
//! | offset | size | field | notes |
//! |--------|------|-------|-------|
//! | 0 | 1 | `version` | must be [`PROTOCOL_VERSION`] (1) |
//! | 1 | 1 | `message_type` | see the `MSG_*` constants |
//! | 2 | 2 | `payload_len` | u16 little-endian |
//! | 4 | 4 | `sequence` | u32 little-endian, per-sender monotonic counter |
//! | 8 | `payload_len` | `payload` | message-specific, see [`Message`] |
//! | 8 + `payload_len` | 4 | `crc32` | u32 little-endian |
//!
//! `crc32` is the standard IEEE CRC-32 (as used by zlib/Ethernet: reflected,
//! polynomial `0xEDB88320`, init and final XOR `0xFFFFFFFF`) computed over
//! every decoded byte that precedes it, i.e. bytes `0 .. 8 + payload_len`.
//!
//! All multi-byte integers are little-endian. The maximum decoded frame size
//! is [`MAX_FRAME_LEN`] (128) bytes; anything larger is rejected.
//!
//! # Field-mode messages
//!
//! Field deployment (`docs/field-protocol.md` §1) adds SetSchedule `0x07`,
//! GetWindow `0x08`, AckWindow `0x09`, GetFieldStatus `0x0A` (host→board)
//! and Sample12 `0x15`, WindowInfo `0x16`, WindowChunk `0x17`, FieldStatus
//! `0x18` (board→host), still protocol version 1. Window data and schedule
//! helpers live in [`field`]. GetRaw `0x0B` / RawChunk `0x19` read the
//! board's raw 25 Hz flash log, whose block format is in [`raw`].
//!
//! # Robustness
//!
//! [`Decoder`] is a push-style streaming decoder intended to be fed raw
//! serial bytes one at a time. It never panics on arbitrary input, never
//! allocates, and resynchronizes at the next `0x00` delimiter after any
//! corrupt, truncated, overlong, or malformed input. Every rejection is
//! reported as a typed [`DecodeError`] so the host can keep error counters.
//!
//! # Example
//!
//! ```
//! use sap_flux_protocol::{encode, Decoder, Message, Query, MAX_ENCODED_LEN};
//!
//! let msg = Message::Query(Query { command_nonce: 42 });
//! let mut buf = [0u8; MAX_ENCODED_LEN];
//! let n = encode(&msg, 7, &mut buf).unwrap();
//!
//! let mut dec = Decoder::new();
//! let mut decoded = None;
//! for &b in &buf[..n] {
//!     if let Some(result) = dec.push(b) {
//!         decoded = Some(result.unwrap());
//!     }
//! }
//! let frame = decoded.unwrap();
//! assert_eq!(frame.sequence, 7);
//! assert_eq!(frame.message, msg);
//! ```

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod cobs;
pub mod crc;
pub mod field;
pub mod raw;

mod frame;
mod msg;

pub use field::{ScheduleError, BIN_VALUE_MISSING, BIN_VALUE_SCALE, SCHEDULE_LEN};
pub use frame::{decode_frame, encode, encode_frame, DecodeError, Decoder, EncodeError, Frame};
pub use msg::{
    payload_len_for, Abort, Ack, AckWindow, ArmPulse, BoardState, ErrorReport, FieldStatus,
    FirePulse, GetFieldStatus, GetRaw, GetWindow, Hello, Message, Query, RawChunk, Sample,
    Sample12, Schedule, SetRtc, SetSchedule, StartBaseline, Status, WindowChunk, WindowInfo,
};

/// Protocol version carried in byte 0 of every frame.
pub const PROTOCOL_VERSION: u8 = 1;

/// Size of the fixed frame header (version, type, payload_len, sequence).
pub const HEADER_LEN: usize = 8;
/// Size of the trailing CRC-32.
pub const CRC_LEN: usize = 4;
/// Smallest possible decoded frame (empty payload).
pub const MIN_FRAME_LEN: usize = HEADER_LEN + CRC_LEN;
/// Largest permitted decoded frame, header and CRC included.
pub const MAX_FRAME_LEN: usize = 128;
/// Largest permitted payload.
pub const MAX_PAYLOAD_LEN: usize = MAX_FRAME_LEN - MIN_FRAME_LEN;
/// Worst-case on-wire size of one frame: COBS overhead for
/// [`MAX_FRAME_LEN`] bytes (1 extra byte below 254) plus the 0x00 delimiter.
pub const MAX_ENCODED_LEN: usize = MAX_FRAME_LEN + 2;

/// Board-to-host: identification, sent on boot and in reply to `Query`.
pub const MSG_HELLO: u8 = 0x10;
/// Board-to-host: one complete ADC sample frame.
pub const MSG_SAMPLE: u8 = 0x11;
/// Board-to-host: periodic state/fault report.
pub const MSG_STATUS: u8 = 0x12;
/// Board-to-host: acknowledgement of a host command.
pub const MSG_ACK: u8 = 0x13;
/// Board-to-host: asynchronous error report.
pub const MSG_ERROR: u8 = 0x14;

/// Host-to-board: request Hello + Status.
pub const MSG_QUERY: u8 = 0x01;
/// Host-to-board: set the RTC.
pub const MSG_SET_RTC: u8 = 0x02;
/// Host-to-board: begin a baseline acquisition run.
pub const MSG_START_BASELINE: u8 = 0x03;
/// Host-to-board: arm the heater pulse.
pub const MSG_ARM_PULSE: u8 = 0x04;
/// Host-to-board: fire a previously armed pulse.
pub const MSG_FIRE_PULSE: u8 = 0x05;
/// Host-to-board: abort the current run.
pub const MSG_ABORT: u8 = 0x06;
/// Host-to-board (field): store the autonomous pulse schedule.
pub const MSG_SET_SCHEDULE: u8 = 0x07;
/// Host-to-board (field): fetch a window header or data chunk.
pub const MSG_GET_WINDOW: u8 = 0x08;
/// Host-to-board (field): mark a window delivered and free it.
pub const MSG_ACK_WINDOW: u8 = 0x09;
/// Host-to-board (field): request a `FieldStatus`.
pub const MSG_GET_FIELD_STATUS: u8 = 0x0A;
/// Host-to-board (field): fetch 100 bytes of a run's raw frame log.
pub const MSG_GET_RAW: u8 = 0x0B;

/// Board-to-host: one complete 12-thermistor sample frame.
pub const MSG_SAMPLE12: u8 = 0x15;
/// Board-to-host (field): stored window header.
pub const MSG_WINDOW_INFO: u8 = 0x16;
/// Board-to-host (field): 100 bytes of window data.
pub const MSG_WINDOW_CHUNK: u8 = 0x17;
/// Board-to-host (field): scheduler, storage and time report.
pub const MSG_FIELD_STATUS: u8 = 0x18;
/// Board-to-host (field): 100 bytes of a run's raw block stream.
pub const MSG_RAW_CHUNK: u8 = 0x19;

/// Ack result: command accepted.
pub const ACK_OK: u8 = 0x00;
/// Ack result: a parameter is out of range (e.g. a `SetSchedule` limit).
pub const ACK_INVALID_PARAMETER: u8 = 0x02;
/// Ack result: `GetWindow` named an unknown run or chunk.
pub const ACK_NOT_FOUND: u8 = 0x04;

/// `GetWindow.run_id` meaning "oldest undelivered window".
pub const WINDOW_OLDEST: u32 = 0;
/// `GetWindow.chunk` meaning "send the `WindowInfo` header".
pub const CHUNK_HEADER: u16 = 0xFFFF;
/// Data bytes per `WindowChunk`.
pub const WINDOW_CHUNK_DATA_LEN: usize = 100;
/// Values per window bin: probe-1 T1..T6, probe-2 T1..T6, excitation.
pub const WINDOW_CHANNELS: usize = 13;
/// Index of the excitation channel within a window bin.
pub const WINDOW_EXCITATION_CHANNEL: usize = 12;
/// Size of a decoded `WindowInfo` frame (header + payload + CRC), the
/// prefix of a reassembled gateway `WindowPart` byte string.
pub const WINDOW_INFO_FRAME_LEN: usize = MIN_FRAME_LEN + 90;

/// `SetSchedule.flags` bit: only pulse while `now < lease_until_unix_s`.
pub const SCHEDULE_FLAG_REQUIRE_LEASE: u8 = 1 << 0;

/// `WindowInfo.window_flags` bit: RTC was invalid; times are monotonic only.
pub const WINDOW_FLAG_TIME_INVALID: u16 = 1 << 0;
/// `WindowInfo.window_flags` bit: ADC errors occurred during the window.
pub const WINDOW_FLAG_ADC_ERRORS: u16 = 1 << 1;
/// `WindowInfo.window_flags` bit: the run was aborted.
pub const WINDOW_FLAG_ABORTED: u16 = 1 << 2;
/// `WindowInfo.window_flags` bit: a heater fault occurred.
pub const WINDOW_FLAG_HEATER_FAULT: u16 = 1 << 3;
/// `WindowInfo.window_flags` bit: the window was cut short.
pub const WINDOW_FLAG_TRUNCATED: u16 = 1 << 4;

/// `FieldStatus.schedule_flags` bit: schedule enabled.
pub const FIELD_STATUS_ENABLED: u8 = 1 << 0;
/// `FieldStatus.schedule_flags` bit: schedule requires a lease.
pub const FIELD_STATUS_REQUIRE_LEASE: u8 = 1 << 1;
/// `FieldStatus.schedule_flags` bit: a lease is currently valid.
pub const FIELD_STATUS_LEASE_VALID: u8 = 1 << 2;
/// `FieldStatus.schedule_flags` bit: RTC is valid.
pub const FIELD_STATUS_RTC_VALID: u8 = 1 << 3;
/// `FieldStatus.schedule_flags` bit: heater is locked out.
pub const FIELD_STATUS_HEATER_LOCKOUT: u8 = 1 << 4;
/// `FieldStatus.schedule_flags` bit: SPI flash is healthy.
pub const FIELD_STATUS_FLASH_OK: u8 = 1 << 5;
/// `FieldStatus.schedule_flags` bit: the EEPROM (heater lockout record)
/// could not be read or written, so the schedule is held.
pub const FIELD_STATUS_EEPROM_FAULT: u8 = 1 << 6;

// Every message must fit in one frame; the largest is RawChunk (112 B).
const _: () = assert!(MIN_FRAME_LEN + 112 <= MAX_FRAME_LEN);

/// Sample flag bit: heater output currently active.
pub const FLAG_HEATER_ACTIVE: u16 = 1 << 0;
/// Sample flag bit: RTC has been set and is considered valid.
pub const FLAG_RTC_VALID: u16 = 1 << 1;
/// Sample flag bit: buck converter PGOOD.
pub const FLAG_PGOOD: u16 = 1 << 2;
/// Sample flag bit: thermistor excitation rail was on for this sample.
pub const FLAG_EXCITATION_ACTIVE: u16 = 1 << 3;
/// Sample flag bit: ADC reported an error for this frame.
pub const FLAG_ADC_ERROR: u16 = 1 << 4;
/// Sample flag bit: this sample opens a host command window.
///
/// The RS-485 link is half-duplex: a host command transmitted while the
/// board is talking collides and is lost. After transmitting a Sample with
/// this bit set, the board stays silent and listening for at least ~40 ms.
/// The host must queue commands and transmit at most one queued command
/// immediately after decoding a Sample carrying this bit — and at no other
/// time. Commands are never retried automatically. This flag only governs
/// the physical half-duplex RS-485 transport; full-duplex TTL UART commands
/// do not need a window, but use the same decoder and safety state machine.
pub const FLAG_COMMAND_WINDOW: u16 = 1 << 5;

/// Hello capability bit: board has a real-time clock.
pub const CAP_RTC: u32 = 1 << 0;
/// Hello capability bit: board has a working thermistor ADC front end.
pub const CAP_ADC: u32 = 1 << 1;
/// Hello capability bit: board monitors the 24 V rail.
pub const CAP_V24_MONITOR: u32 = 1 << 2;
/// Hello capability bit: firmware supports heater control (StartBaseline,
/// ArmPulse, FirePulse). Heater-incapable firmware omits this bit; hosts
/// must disable heater controls when it is absent.
pub const CAP_HEATER_CONTROL: u32 = 1 << 3;
