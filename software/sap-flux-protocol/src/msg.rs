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

/// The 20 schedule bytes shared by `SetSchedule` (bytes 8..28 of its
/// payload), the server's `CheckInReply.schedule`, and `schedule_crc`.
///
/// Layout: `enabled: u8`, `flags: u8`, `pulse_duration_ms: u16`,
/// `interval_s: u32`, `phase_s: u32`, `baseline_s: u16`, `recovery_s: u16`,
/// `sample_hz: u16`, `bin_ms: u16`.
///
/// Pulses start at RTC times `t` (Unix seconds) with
/// `(t - phase_s) mod interval_s == 0`; `t` is the heater-on time and the
/// baseline starts `baseline_s` earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    /// 1 = pulse on this schedule, 0 = disabled. Kept as the raw byte so a
    /// decoded schedule re-encodes (and CRCs) bit-exactly.
    pub enabled: u8,
    /// `SCHEDULE_FLAG_*` bits; all other bits must be zero.
    pub flags: u8,
    /// Heater pulse length: 500, 1000 or 1500 ms.
    pub pulse_duration_ms: u16,
    /// Pulse period in seconds (≥ 600).
    pub interval_s: u32,
    /// Pulse phase offset in seconds (see struct docs).
    pub phase_s: u32,
    /// Pre-pulse baseline length in seconds (10–300).
    pub baseline_s: u16,
    /// Post-pulse recovery length in seconds (60–600).
    pub recovery_s: u16,
    /// Complete-frame sample rate: 10, 20 or 25 Hz.
    pub sample_hz: u16,
    /// Uplink bin width: 250, 500 or 1000 ms.
    pub bin_ms: u16,
}

/// `0x07` SetSchedule — store the autonomous pulse schedule (32-byte
/// payload).
///
/// Layout: `command_nonce: u64`, then the 20 [`Schedule`] bytes, then
/// `lease_until_unix_s: u32`. Reply: `Ack` (result 0 or a code, e.g.
/// [`ACK_INVALID_PARAMETER`](crate::ACK_INVALID_PARAMETER)) then
/// `FieldStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetSchedule {
    /// Host-chosen nonce echoed in the Ack.
    pub command_nonce: u64,
    /// The schedule itself.
    pub schedule: Schedule,
    /// With [`SCHEDULE_FLAG_REQUIRE_LEASE`](crate::SCHEDULE_FLAG_REQUIRE_LEASE),
    /// the board only pulses while `now < lease_until_unix_s`.
    pub lease_until_unix_s: u32,
}

/// `0x08` GetWindow — fetch a stored pulse window's header or one data
/// chunk (14-byte payload).
///
/// Layout: `command_nonce: u64`, `run_id: u32`
/// ([`WINDOW_OLDEST`](crate::WINDOW_OLDEST) = 0 = oldest undelivered),
/// `chunk: u16` ([`CHUNK_HEADER`](crate::CHUNK_HEADER) = `0xFFFF` = header).
/// Reply: `WindowInfo` for the header, else `WindowChunk`; unknown run or
/// chunk gets `Ack` with [`ACK_NOT_FOUND`](crate::ACK_NOT_FOUND).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetWindow {
    /// Host-chosen nonce echoed in a NOT_FOUND Ack.
    pub command_nonce: u64,
    /// Run id of the window; 0 = oldest undelivered.
    pub run_id: u32,
    /// Data chunk index, or `0xFFFF` for the `WindowInfo` header.
    pub chunk: u16,
}

/// `0x09` AckWindow — mark a window delivered and free it (12-byte
/// payload). Layout: `command_nonce: u64`, `run_id: u32`. Reply: `Ack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckWindow {
    /// Host-chosen nonce echoed in the Ack.
    pub command_nonce: u64,
    /// Run id of the delivered window.
    pub run_id: u32,
}

/// `0x0A` GetFieldStatus — request a `FieldStatus` (8-byte payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetFieldStatus {
    /// Host-chosen nonce.
    pub command_nonce: u64,
}

/// `0x15` Sample12 — one complete 12-thermistor ADC frame (52-byte payload),
/// for bench streaming and the monitor.
///
/// Layout: `monotonic_us: u64`, `rtc_unix_ms: i64`, `run_id: u32`,
/// `flags: u16`, `state: u8`, `reserved: u8` (sent as zero, ignored on
/// receive), `excitation_raw: i16`, `v24_raw: i16`,
/// `thermistor_raw: [i16; 12]` (probe 1 T1..T6, then probe 2 T1..T6).
/// Note the field order differs from [`Sample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample12 {
    /// Microseconds since boot (monotonic).
    pub monotonic_us: u64,
    /// RTC time in Unix milliseconds; `-1` means the RTC is invalid.
    pub rtc_unix_ms: i64,
    /// Run this sample belongs to (0 when idle).
    pub run_id: u32,
    /// `FLAG_*` bits (same meaning as `Sample.flags`), see crate root.
    pub flags: u16,
    /// Instrument state when the sample was taken.
    pub state: BoardState,
    /// Raw ADC counts on the excitation-sense channel.
    pub excitation_raw: i16,
    /// Raw ADC counts on the 24 V rail monitor.
    pub v24_raw: i16,
    /// Raw ADC counts: probe 1 T1..T6, then probe 2 T1..T6.
    pub thermistor_raw: [i16; 12],
}

/// `0x16` WindowInfo — header of one stored pulse window (90-byte payload).
///
/// Layout: `run_id: u32`, `window_flags: u16`, `channels: u8` (13),
/// `bin_ms_div10: u8` (50 = 500 ms), `n_bins: u16`, `chunk_count: u16`,
/// `start_monotonic_us: u64`, `start_rtc_unix_ms: i64`,
/// `end_monotonic_us: u64`, `end_rtc_unix_ms: i64`,
/// `heater_on_offset_ms: u32` (from window start), `pulse_duration_ms: u16`,
/// `sample_hz: u16`, `v24_mean_raw: i16`, `frames_total: u16`,
/// `frames_missing: u16`, `data_crc32: u32`, `base_raw: [i16; 13]`,
/// `reserved: u16` (sent as zero, ignored on receive).
///
/// The window data that follows (in `WindowChunk`s) is `n_bins` bins of
/// `channels` × `i16`, see [`crate::field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowInfo {
    /// Run id of this window.
    pub run_id: u32,
    /// `WINDOW_FLAG_*` bits, see crate root.
    pub window_flags: u16,
    /// Values per bin; 13 (12 thermistors + excitation).
    pub channels: u8,
    /// Bin width in units of 10 ms (50 = 500 ms).
    pub bin_ms_div10: u8,
    /// Number of bins in the window data.
    pub n_bins: u16,
    /// Number of `WindowChunk`s: `ceil(data_len / 100)`.
    pub chunk_count: u16,
    /// Monotonic time of the window start (bin 0 start).
    pub start_monotonic_us: u64,
    /// RTC time of the window start; `-1` if invalid.
    pub start_rtc_unix_ms: i64,
    /// Monotonic time of the window end.
    pub end_monotonic_us: u64,
    /// RTC time of the window end; `-1` if invalid.
    pub end_rtc_unix_ms: i64,
    /// Heater-on time, ms after the window start.
    pub heater_on_offset_ms: u32,
    /// Pulse length actually applied.
    pub pulse_duration_ms: u16,
    /// Complete-frame sample rate used.
    pub sample_hz: u16,
    /// Mean raw 24 V monitor reading over the window.
    pub v24_mean_raw: i16,
    /// Frames expected over the window.
    pub frames_total: u16,
    /// Frames missing (ADC errors / overruns).
    pub frames_missing: u16,
    /// Protocol CRC-32 over the full window data byte string.
    pub data_crc32: u32,
    /// Per-channel base: rounded mean raw count in the first bin.
    pub base_raw: [i16; 13],
}

/// `0x17` WindowChunk — 100 bytes of window data (108-byte payload).
///
/// Layout: `run_id: u32`, `chunk: u16`, `len: u16` (≤ 100),
/// `data: [u8; 100]` (bytes past `len` are zero on send, ignored on
/// receive). Chunk `i` holds data bytes `[100·i, 100·i + len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowChunk {
    /// Run id of the window.
    pub run_id: u32,
    /// Chunk index.
    pub chunk: u16,
    /// Valid bytes in `data` (≤ 100).
    pub len: u16,
    /// Chunk bytes; only `data[..len]` is meaningful.
    pub data: [u8; crate::WINDOW_CHUNK_DATA_LEN],
}

impl WindowChunk {
    /// Build a chunk from up to 100 data bytes (zero-padded). Returns
    /// `None` if `bytes` is longer than 100.
    pub fn new(run_id: u32, chunk: u16, bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::WINDOW_CHUNK_DATA_LEN {
            return None;
        }
        let mut data = [0u8; crate::WINDOW_CHUNK_DATA_LEN];
        data[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            run_id,
            chunk,
            len: bytes.len() as u16,
            data,
        })
    }

    /// The meaningful `data[..len]` bytes.
    pub fn bytes(&self) -> &[u8] {
        let n = (self.len as usize).min(crate::WINDOW_CHUNK_DATA_LEN);
        &self.data[..n]
    }
}

/// `0x18` FieldStatus — field-mode scheduler/storage/time report (52-byte
/// payload).
///
/// Layout: `monotonic_us: u64`, `rtc_unix_ms: i64`,
/// `next_pulse_unix_s: u32`, `schedule_flags: u8`, `windows_pending: u8`,
/// `windows_dropped: u16`, `oldest_pending_run_id: u32`, `last_run_id: u32`,
/// `calr_ppb: i32`, `last_rtc_step_ms: i32`, `boot_count: u16`,
/// `v24_raw: i16`, `flash_used_kib: u32`, `schedule_crc: u16`,
/// `reserved: u16` (sent as zero, ignored on receive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldStatus {
    /// Microseconds since boot (monotonic).
    pub monotonic_us: u64,
    /// RTC time in Unix milliseconds; `-1` means the RTC is invalid.
    pub rtc_unix_ms: i64,
    /// Next scheduled heater-on time (Unix s); 0 if disabled.
    pub next_pulse_unix_s: u32,
    /// `FIELD_STATUS_*` bits, see crate root.
    pub schedule_flags: u8,
    /// Undelivered windows held by the board.
    pub windows_pending: u8,
    /// Windows overwritten before delivery (since boot).
    pub windows_dropped: u16,
    /// Run id of the oldest undelivered window (0 if none).
    pub oldest_pending_run_id: u32,
    /// Run id of the most recent window.
    pub last_run_id: u32,
    /// Current RTC trim in parts per billion; positive = slowed.
    pub calr_ppb: i32,
    /// Last `SetRtc` correction applied, new − old, in ms.
    pub last_rtc_step_ms: i32,
    /// Board boot counter.
    pub boot_count: u16,
    /// Raw 24 V rail monitor reading.
    pub v24_raw: i16,
    /// SPI flash used, KiB.
    pub flash_used_kib: u32,
    /// [`Schedule::crc16`] of the last accepted schedule.
    pub schedule_crc: u16,
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
    /// `0x07`
    SetSchedule(SetSchedule),
    /// `0x08`
    GetWindow(GetWindow),
    /// `0x09`
    AckWindow(AckWindow),
    /// `0x0A`
    GetFieldStatus(GetFieldStatus),
    /// `0x15`
    Sample12(Sample12),
    /// `0x16`
    WindowInfo(WindowInfo),
    /// `0x17`
    WindowChunk(WindowChunk),
    /// `0x18`
    FieldStatus(FieldStatus),
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
            Message::SetSchedule(_) => crate::MSG_SET_SCHEDULE,
            Message::GetWindow(_) => crate::MSG_GET_WINDOW,
            Message::AckWindow(_) => crate::MSG_ACK_WINDOW,
            Message::GetFieldStatus(_) => crate::MSG_GET_FIELD_STATUS,
            Message::Sample12(_) => crate::MSG_SAMPLE12,
            Message::WindowInfo(_) => crate::MSG_WINDOW_INFO,
            Message::WindowChunk(_) => crate::MSG_WINDOW_CHUNK,
            Message::FieldStatus(_) => crate::MSG_FIELD_STATUS,
        }
    }

    /// Exact payload size in bytes for this message.
    pub fn payload_len(&self) -> usize {
        match payload_len_for(self.message_type()) {
            Some(n) => n,
            None => unreachable!("every Message variant has a size"),
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
            Message::SetSchedule(m) => {
                w.u64(m.command_nonce);
                w.put(&m.schedule.to_bytes());
                w.u32(m.lease_until_unix_s);
            }
            Message::GetWindow(m) => {
                w.u64(m.command_nonce);
                w.u32(m.run_id);
                w.u16(m.chunk);
            }
            Message::AckWindow(m) => {
                w.u64(m.command_nonce);
                w.u32(m.run_id);
            }
            Message::GetFieldStatus(m) => {
                w.u64(m.command_nonce);
            }
            Message::Sample12(m) => {
                w.u64(m.monotonic_us);
                w.i64(m.rtc_unix_ms);
                w.u32(m.run_id);
                w.u16(m.flags);
                w.u8(m.state as u8);
                w.u8(0); // reserved
                w.i16(m.excitation_raw);
                w.i16(m.v24_raw);
                for &t in &m.thermistor_raw {
                    w.i16(t);
                }
            }
            Message::WindowInfo(m) => {
                w.u32(m.run_id);
                w.u16(m.window_flags);
                w.u8(m.channels);
                w.u8(m.bin_ms_div10);
                w.u16(m.n_bins);
                w.u16(m.chunk_count);
                w.u64(m.start_monotonic_us);
                w.i64(m.start_rtc_unix_ms);
                w.u64(m.end_monotonic_us);
                w.i64(m.end_rtc_unix_ms);
                w.u32(m.heater_on_offset_ms);
                w.u16(m.pulse_duration_ms);
                w.u16(m.sample_hz);
                w.i16(m.v24_mean_raw);
                w.u16(m.frames_total);
                w.u16(m.frames_missing);
                w.u32(m.data_crc32);
                for &b in &m.base_raw {
                    w.i16(b);
                }
                w.u16(0); // reserved
            }
            Message::WindowChunk(m) => {
                w.u32(m.run_id);
                w.u16(m.chunk);
                w.u16(m.len);
                // Bytes past `len` go out as zero padding.
                let n = (m.len as usize).min(crate::WINDOW_CHUNK_DATA_LEN);
                let mut data = [0u8; crate::WINDOW_CHUNK_DATA_LEN];
                data[..n].copy_from_slice(&m.data[..n]);
                w.put(&data);
            }
            Message::FieldStatus(m) => {
                w.u64(m.monotonic_us);
                w.i64(m.rtc_unix_ms);
                w.u32(m.next_pulse_unix_s);
                w.u8(m.schedule_flags);
                w.u8(m.windows_pending);
                w.u16(m.windows_dropped);
                w.u32(m.oldest_pending_run_id);
                w.u32(m.last_run_id);
                w.i32(m.calr_ppb);
                w.i32(m.last_rtc_step_ms);
                w.u16(m.boot_count);
                w.i16(m.v24_raw);
                w.u32(m.flash_used_kib);
                w.u16(m.schedule_crc);
                w.u16(0); // reserved
            }
        }
        debug_assert_eq!(w.pos, buf.len());
    }

    /// Parse a payload for a known `message_type`.
    pub(crate) fn parse_payload(message_type: u8, payload: &[u8]) -> Result<Message, DecodeError> {
        let expected = match payload_len_for(message_type) {
            Some(n) => n,
            None => return Err(DecodeError::UnknownMessageType(message_type)),
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
            crate::MSG_SET_SCHEDULE => Message::SetSchedule(SetSchedule {
                command_nonce: r.u64(),
                schedule: Schedule::from_bytes(&r.bytes()),
                lease_until_unix_s: r.u32(),
            }),
            crate::MSG_GET_WINDOW => Message::GetWindow(GetWindow {
                command_nonce: r.u64(),
                run_id: r.u32(),
                chunk: r.u16(),
            }),
            crate::MSG_ACK_WINDOW => Message::AckWindow(AckWindow {
                command_nonce: r.u64(),
                run_id: r.u32(),
            }),
            crate::MSG_GET_FIELD_STATUS => Message::GetFieldStatus(GetFieldStatus {
                command_nonce: r.u64(),
            }),
            crate::MSG_SAMPLE12 => {
                let monotonic_us = r.u64();
                let rtc_unix_ms = r.i64();
                let run_id = r.u32();
                let flags = r.u16();
                let state_byte = r.u8();
                let _reserved = r.u8(); // ignored on receive
                let state =
                    BoardState::from_u8(state_byte).ok_or(DecodeError::InvalidState(state_byte))?;
                let excitation_raw = r.i16();
                let v24_raw = r.i16();
                let mut thermistor_raw = [0i16; 12];
                for t in &mut thermistor_raw {
                    *t = r.i16();
                }
                Message::Sample12(Sample12 {
                    monotonic_us,
                    rtc_unix_ms,
                    run_id,
                    flags,
                    state,
                    excitation_raw,
                    v24_raw,
                    thermistor_raw,
                })
            }
            crate::MSG_WINDOW_INFO => {
                let mut m = WindowInfo {
                    run_id: r.u32(),
                    window_flags: r.u16(),
                    channels: r.u8(),
                    bin_ms_div10: r.u8(),
                    n_bins: r.u16(),
                    chunk_count: r.u16(),
                    start_monotonic_us: r.u64(),
                    start_rtc_unix_ms: r.i64(),
                    end_monotonic_us: r.u64(),
                    end_rtc_unix_ms: r.i64(),
                    heater_on_offset_ms: r.u32(),
                    pulse_duration_ms: r.u16(),
                    sample_hz: r.u16(),
                    v24_mean_raw: r.i16(),
                    frames_total: r.u16(),
                    frames_missing: r.u16(),
                    data_crc32: r.u32(),
                    base_raw: [0; 13],
                };
                for b in &mut m.base_raw {
                    *b = r.i16();
                }
                let _reserved = r.u16(); // ignored on receive
                Message::WindowInfo(m)
            }
            crate::MSG_WINDOW_CHUNK => {
                let run_id = r.u32();
                let chunk = r.u16();
                let len = r.u16();
                if len as usize > crate::WINDOW_CHUNK_DATA_LEN {
                    return Err(DecodeError::ChunkLength(len));
                }
                let mut data: [u8; crate::WINDOW_CHUNK_DATA_LEN] = r.bytes();
                // Padding past `len` is ignored on receive; normalize it to
                // zero so equal chunks compare equal.
                data[len as usize..].fill(0);
                Message::WindowChunk(WindowChunk {
                    run_id,
                    chunk,
                    len,
                    data,
                })
            }
            crate::MSG_FIELD_STATUS => {
                let m = FieldStatus {
                    monotonic_us: r.u64(),
                    rtc_unix_ms: r.i64(),
                    next_pulse_unix_s: r.u32(),
                    schedule_flags: r.u8(),
                    windows_pending: r.u8(),
                    windows_dropped: r.u16(),
                    oldest_pending_run_id: r.u32(),
                    last_run_id: r.u32(),
                    calr_ppb: r.i32(),
                    last_rtc_step_ms: r.i32(),
                    boot_count: r.u16(),
                    v24_raw: r.i16(),
                    flash_used_kib: r.u32(),
                    schedule_crc: r.u16(),
                };
                let _reserved = r.u16(); // ignored on receive
                Message::FieldStatus(m)
            }
            _ => unreachable!("length table above rejects unknown types"),
        };
        debug_assert_eq!(r.pos, payload.len());
        Ok(msg)
    }
}

/// Fixed payload size for a protocol v1 `message_type`, or `None` if the
/// type is not defined.
pub fn payload_len_for(message_type: u8) -> Option<usize> {
    Some(match message_type {
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
        crate::MSG_SET_SCHEDULE => 32,
        crate::MSG_GET_WINDOW => 14,
        crate::MSG_ACK_WINDOW => 12,
        crate::MSG_GET_FIELD_STATUS => 8,
        crate::MSG_SAMPLE12 => 52,
        crate::MSG_WINDOW_INFO => 90,
        crate::MSG_WINDOW_CHUNK => 108,
        crate::MSG_FIELD_STATUS => 52,
        _ => return None,
    })
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
    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.bytes())
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
    fn i32(&mut self, v: i32) {
        self.put(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.put(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.put(&v.to_le_bytes());
    }
}
