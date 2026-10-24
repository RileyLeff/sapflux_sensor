//! Strongly typed protocol messages and their payload encodings.
//!
//! All payload fields are little-endian. Payload layouts are documented on
//! each struct; sizes are fixed per message type.

use crate::frame::DecodeError;

/// Instrument state machine, as reported in `Sample.state` / `Status.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BoardState {
    /// Heater locked out, idle sampling only.
    SafeIdle = 0,
    /// Pre-pulse baseline acquisition in progress.
    Baseline = 1,
    /// Pulse armed, waiting for fire command.
    Armed = 2,
    /// Heater pulse in progress.
    Pulsing = 3,
    /// Post-pulse recovery acquisition in progress.
    Recovery = 4,
    /// Mandatory cooldown before another pulse is permitted.
    Cooldown = 5,
    /// Latched fault; requires intervention.
    Fault = 6,
}

impl BoardState {
    /// Parse a state byte, rejecting unknown values.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::SafeIdle,
            1 => Self::Baseline,
            2 => Self::Armed,
            3 => Self::Pulsing,
            4 => Self::Recovery,
            5 => Self::Cooldown,
            6 => Self::Fault,
            _ => return None,
        })
    }
}

/// `0x10` Hello — board identification (16-byte payload).
///
/// Layout: `board_id: u64`, `firmware_version: u32`, `capabilities: u32`.
/// `firmware_version` packs `major << 16 | minor << 8 | patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    /// Unique board identifier (e.g. from the STM32 UID).
    pub board_id: u64,
    /// Packed firmware version: `major << 16 | minor << 8 | patch`.
    pub firmware_version: u32,
    /// Capability bit mask (`CAP_*` constants, see crate root).
    pub capabilities: u32,
}

/// `0x11` Sample — one complete ADC frame (40-byte payload).
///
/// Layout: `monotonic_us: u64`, `rtc_unix_ms: i64`, `run_id: u32`,
/// `flags: u16`, `excitation_raw: i16`, `thermistor_raw: [i16; 6]` (T1..T6),
/// `v24_raw: i16`, `state: u8`, `reserved: u8` (sent as zero, ignored on
/// receive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Microseconds since boot (monotonic).
    pub monotonic_us: u64,
    /// RTC time in Unix milliseconds; `-1` means the RTC is invalid.
    pub rtc_unix_ms: i64,
    /// Run this sample belongs to (0 when idle).
    pub run_id: u32,
    /// `FLAG_*` bits, see crate root.
    pub flags: u16,
    /// Raw ADC counts on the excitation-sense channel.
    pub excitation_raw: i16,
    /// Raw ADC counts for thermistors T1..T6, in order.
    pub thermistor_raw: [i16; 6],
    /// Raw ADC counts on the 24 V rail monitor.
    pub v24_raw: i16,
    /// Instrument state when the sample was taken.
    pub state: BoardState,
}

/// `0x12` Status — periodic state report (28-byte payload).
///
/// Layout: `monotonic_us: u64`, `rtc_unix_ms: i64`, `run_id: u32`,
/// `state: u8`, `fault_code: u8`, `pulse_duration_ms: u16`,
/// `cooldown_remaining_ms: u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Microseconds since boot (monotonic).
    pub monotonic_us: u64,
    /// RTC time in Unix milliseconds; `-1` means the RTC is invalid.
    pub rtc_unix_ms: i64,
    /// Current run id (0 when idle).
    pub run_id: u32,
    /// Current instrument state.
    pub state: BoardState,
    /// Nonzero when a fault is latched.
    pub fault_code: u8,
    /// Configured pulse duration for the current/last run.
    pub pulse_duration_ms: u16,
    /// Board-authoritative cooldown remaining before the next pulse.
    pub cooldown_remaining_ms: u32,
}

/// `0x13` Ack — command acknowledgement (16-byte payload).
///
/// Layout: `command_type: u8`, `result: u8`, `reserved: u16` (zero),
/// `command_nonce: u64`, `run_id: u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    /// Message type of the command being acknowledged.
    pub command_type: u8,
    /// 0 = accepted; nonzero = rejection/error code.
    pub result: u8,
    /// Nonce copied from the acknowledged command.
    pub command_nonce: u64,
    /// Run id associated with the command.
    pub run_id: u32,
}

/// `0x14` Error — asynchronous error report (4-byte payload).
///
/// Layout: `fault_code: u16`, `detail: u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorReport {
    /// Fault code.
    pub fault_code: u16,
    /// Fault-specific detail.
    pub detail: u16,
}

/// `0x01` Query — request Hello + Status (8-byte payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query {
    /// Host-chosen nonce echoed in the Ack.
    pub command_nonce: u64,
}

/// `0x02` SetRtc — set the board RTC (16-byte payload).
///
/// Layout: `command_nonce: u64`, `unix_ms: i64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetRtc {
    /// Host-chosen nonce echoed in the Ack.
    pub command_nonce: u64,
    /// New RTC value in Unix milliseconds.
    pub unix_ms: i64,
}

/// `0x03` StartBaseline — begin a run (22-byte payload).
///
/// Layout: `command_nonce: u64`, `run_id: u32`, `baseline_duration_ms: u32`,
/// `recovery_duration_ms: u32`, `requested_sample_hz: u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartBaseline {
    /// Host-chosen nonce echoed in the Ack.
    pub command_nonce: u64,
    /// Host-chosen run id for the new run.
    pub run_id: u32,
    /// Pre-pulse baseline duration.
    pub baseline_duration_ms: u32,
    /// Post-pulse recovery duration.
    pub recovery_duration_ms: u32,
    /// Requested complete-frame sample rate.
    pub requested_sample_hz: u16,
}

/// `0x04` ArmPulse — arm the heater pulse (14-byte payload).
///
/// Layout: `command_nonce: u64`, `run_id: u32`, `pulse_duration_ms: u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmPulse {
    /// Host-chosen nonce echoed in the Ack.
    pub command_nonce: u64,
    /// Run id the arm applies to; must match the board's active run.
    pub run_id: u32,
    /// Pulse duration; the host UI only permits 500, 1000 or 1500.
    pub pulse_duration_ms: u16,
}

/// `0x05` FirePulse — fire a previously armed pulse (12-byte payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirePulse {
    /// Host-chosen nonce echoed in the Ack. One-use.
    pub command_nonce: u64,
    /// Run id the fire applies to; must match the armed run.
    pub run_id: u32,
}

/// `0x06` Abort — abort the current run (12-byte payload).
///
/// Note: an abort cannot shorten a hardware pulse already in progress; it
/// stops the run at the earliest safe opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abort {
    /// Host-chosen nonce echoed in the Ack.
    pub command_nonce: u64,
    /// Run id to abort.
    pub run_id: u32,
}

/// Any protocol v1 message, board-to-host or host-to-board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// `0x10`
    Hello(Hello),
    /// `0x11`
    Sample(Sample),
    /// `0x12`
    Status(Status),
    /// `0x13`
    Ack(Ack),
    /// `0x14`
    Error(ErrorReport),
    /// `0x01`
    Query(Query),
    /// `0x02`
    SetRtc(SetRtc),
    /// `0x03`
    StartBaseline(StartBaseline),
    /// `0x04`
    ArmPulse(ArmPulse),
    /// `0x05`
    FirePulse(FirePulse),
    /// `0x06`
    Abort(Abort),
}

impl Message {
    /// The wire `message_type` byte for this message.
    pub fn message_type(&self) -> u8 {
        match self {
            Message::Hello(_) => crate::MSG_HELLO,
            Message::Sample(_) => crate::MSG_SAMPLE,
            Message::Status(_) => crate::MSG_STATUS,
            Message::Ack(_) => crate::MSG_ACK,
            Message::Error(_) => crate::MSG_ERROR,
            Message::Query(_) => crate::MSG_QUERY,
            Message::SetRtc(_) => crate::MSG_SET_RTC,
            Message::StartBaseline(_) => crate::MSG_START_BASELINE,
            Message::ArmPulse(_) => crate::MSG_ARM_PULSE,
            Message::FirePulse(_) => crate::MSG_FIRE_PULSE,
            Message::Abort(_) => crate::MSG_ABORT,
        }
    }

    /// Exact payload size in bytes for this message.
    pub fn payload_len(&self) -> usize {
        match self {
            Message::Hello(_) => 16,
            Message::Sample(_) => 40,
            Message::Status(_) => 28,
            Message::Ack(_) => 16,
            Message::Error(_) => 4,
            Message::Query(_) => 8,
            Message::SetRtc(_) => 16,
            Message::StartBaseline(_) => 22,
            Message::ArmPulse(_) => 14,
            Message::FirePulse(_) => 12,
            Message::Abort(_) => 12,
        }
    }

    /// Serialize the payload into `buf`, which must be exactly
    /// [`payload_len`](Self::payload_len) bytes.
    pub(crate) fn write_payload(&self, buf: &mut [u8]) {
        let mut w = Writer { buf, pos: 0 };
        match self {
            Message::Hello(m) => {
                w.u64(m.board_id);
                w.u32(m.firmware_version);
                w.u32(m.capabilities);
            }
            Message::Sample(m) => {
                w.u64(m.monotonic_us);
                w.i64(m.rtc_unix_ms);
                w.u32(m.run_id);
                w.u16(m.flags);
                w.i16(m.excitation_raw);
                for &t in &m.thermistor_raw {
                    w.i16(t);
                }
                w.i16(m.v24_raw);
                w.u8(m.state as u8);
                w.u8(0); // reserved
            }
            Message::Status(m) => {
                w.u64(m.monotonic_us);
                w.i64(m.rtc_unix_ms);
                w.u32(m.run_id);
                w.u8(m.state as u8);
                w.u8(m.fault_code);
                w.u16(m.pulse_duration_ms);
                w.u32(m.cooldown_remaining_ms);
            }
            Message::Ack(m) => {
                w.u8(m.command_type);
                w.u8(m.result);
                w.u16(0); // reserved
                w.u64(m.command_nonce);
                w.u32(m.run_id);
            }
            Message::Error(m) => {
                w.u16(m.fault_code);
                w.u16(m.detail);
            }
            Message::Query(m) => {
                w.u64(m.command_nonce);
            }
            Message::SetRtc(m) => {
                w.u64(m.command_nonce);
                w.i64(m.unix_ms);
            }
            Message::StartBaseline(m) => {
                w.u64(m.command_nonce);
                w.u32(m.run_id);
                w.u32(m.baseline_duration_ms);
                w.u32(m.recovery_duration_ms);
                w.u16(m.requested_sample_hz);
            }
            Message::ArmPulse(m) => {
                w.u64(m.command_nonce);
                w.u32(m.run_id);
                w.u16(m.pulse_duration_ms);
            }
            Message::FirePulse(m) => {
                w.u64(m.command_nonce);
                w.u32(m.run_id);
            }
            Message::Abort(m) => {
                w.u64(m.command_nonce);
                w.u32(m.run_id);
            }
        }
        debug_assert_eq!(w.pos, buf.len());
    }

    /// Parse a payload for a known `message_type`.
    pub(crate) fn parse_payload(message_type: u8, payload: &[u8]) -> Result<Message, DecodeError> {
        let expected = match message_type {
            crate::MSG_HELLO => 16,
            crate::MSG_SAMPLE => 40,
            crate::MSG_STATUS => 28,
            crate::MSG_ACK => 16,
            crate::MSG_ERROR => 4,
            crate::MSG_QUERY => 8,
            crate::MSG_SET_RTC => 16,
            crate::MSG_START_BASELINE => 22,
            crate::MSG_ARM_PULSE => 14,
            crate::MSG_FIRE_PULSE => 12,
            crate::MSG_ABORT => 12,
            other => return Err(DecodeError::UnknownMessageType(other)),
        };
        if payload.len() != expected {
            return Err(DecodeError::PayloadLength {
                message_type,
                expected,
                actual: payload.len(),
            });
        }
        let mut r = Reader {
            buf: payload,
            pos: 0,
        };
        let msg = match message_type {
            crate::MSG_HELLO => Message::Hello(Hello {
                board_id: r.u64(),
                firmware_version: r.u32(),
                capabilities: r.u32(),
            }),
            crate::MSG_SAMPLE => {
                let monotonic_us = r.u64();
                let rtc_unix_ms = r.i64();
                let run_id = r.u32();
                let flags = r.u16();
                let excitation_raw = r.i16();
                let mut thermistor_raw = [0i16; 6];
                for t in &mut thermistor_raw {
                    *t = r.i16();
                }
                let v24_raw = r.i16();
                let state_byte = r.u8();
                let _reserved = r.u8(); // ignored on receive
                let state =
                    BoardState::from_u8(state_byte).ok_or(DecodeError::InvalidState(state_byte))?;
                Message::Sample(Sample {
                    monotonic_us,
                    rtc_unix_ms,
                    run_id,
                    flags,
                    excitation_raw,
                    thermistor_raw,
                    v24_raw,
                    state,
                })
            }
            crate::MSG_STATUS => {
                let monotonic_us = r.u64();
                let rtc_unix_ms = r.i64();
                let run_id = r.u32();
                let state_byte = r.u8();
                let state =
                    BoardState::from_u8(state_byte).ok_or(DecodeError::InvalidState(state_byte))?;
                Message::Status(Status {
                    monotonic_us,
                    rtc_unix_ms,
                    run_id,
                    state,
                    fault_code: r.u8(),
                    pulse_duration_ms: r.u16(),
                    cooldown_remaining_ms: r.u32(),
                })
            }
            crate::MSG_ACK => {
                let command_type = r.u8();
                let result = r.u8();
                let _reserved = r.u16(); // ignored on receive
                Message::Ack(Ack {
                    command_type,
                    result,
                    command_nonce: r.u64(),
                    run_id: r.u32(),
                })
            }
            crate::MSG_ERROR => Message::Error(ErrorReport {
                fault_code: r.u16(),
                detail: r.u16(),
            }),
            crate::MSG_QUERY => Message::Query(Query {
                command_nonce: r.u64(),
            }),
            crate::MSG_SET_RTC => Message::SetRtc(SetRtc {
                command_nonce: r.u64(),
                unix_ms: r.i64(),
            }),
            crate::MSG_START_BASELINE => Message::StartBaseline(StartBaseline {
                command_nonce: r.u64(),
                run_id: r.u32(),
                baseline_duration_ms: r.u32(),
                recovery_duration_ms: r.u32(),
                requested_sample_hz: r.u16(),
            }),
            crate::MSG_ARM_PULSE => Message::ArmPulse(ArmPulse {
                command_nonce: r.u64(),
                run_id: r.u32(),
                pulse_duration_ms: r.u16(),
            }),
            crate::MSG_FIRE_PULSE => Message::FirePulse(FirePulse {
                command_nonce: r.u64(),
                run_id: r.u32(),
            }),
            crate::MSG_ABORT => Message::Abort(Abort {
                command_nonce: r.u64(),
                run_id: r.u32(),
            }),
            _ => unreachable!("length table above rejects unknown types"),
        };
        debug_assert_eq!(r.pos, payload.len());
        Ok(msg)
    }
}

/// Little-endian cursor over a length-verified payload slice.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        // Payload length is verified against the per-type size before any
        // read, so this slice is always in bounds.
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        out
    }
    fn u8(&mut self) -> u8 {
        let [b] = self.bytes::<1>();
        b
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.bytes())
    }
    fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.bytes())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.bytes())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.bytes())
    }
    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.bytes())
    }
}

struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) {
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
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
