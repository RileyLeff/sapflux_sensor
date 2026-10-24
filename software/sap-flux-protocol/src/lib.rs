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

mod frame;
mod msg;

pub use frame::{decode_frame, encode, DecodeError, Decoder, EncodeError, Frame};
pub use msg::{
    Abort, Ack, ArmPulse, BoardState, ErrorReport, FirePulse, Hello, Message, Query, Sample,
    SetRtc, StartBaseline, Status,
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
