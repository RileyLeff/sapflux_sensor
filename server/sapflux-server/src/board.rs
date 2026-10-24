//! Board v1 frames as carried inside uplink datagrams (decoded frames:
//! header + payload + CRC-32, no COBS).
//!
//! All decoding is done by `sap-flux-protocol`; this module only adds
//! serde-friendly mirrors of its structs and flag names for storage, the
//! API and the CLI.

use sap_flux_protocol::{crc::crc32, decode_frame, DecodeError, Message, CRC_LEN, HEADER_LEN};
use serde::{Deserialize, Serialize};

pub use sap_flux_protocol::{
    MSG_FIELD_STATUS, MSG_HELLO, MSG_STATUS, MSG_WINDOW_INFO, WINDOW_FLAG_TIME_INVALID,
    WINDOW_INFO_FRAME_LEN,
};

const WINDOW_FLAG_NAMES: [&str; 5] = [
    "TIME_INVALID",
    "ADC_ERRORS",
    "ABORTED",
    "HEATER_FAULT",
    "TRUNCATED",
];
const SCHEDULE_FLAG_NAMES: [&str; 7] = [
    "enabled",
    "require_lease",
    "lease_valid",
    "rtc_valid",
    "heater_lockout",
    "flash_ok",
    "eeprom_fault",
];
const STATE_NAMES: [&str; 7] = [
    "SafeIdle", "Baseline", "Armed", "Pulsing", "Recovery", "Cooldown", "Fault",
];

fn flag_names(bits: u32, names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = names
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & (1 << i) != 0)
        .map(|(_, n)| n.to_string())
        .collect();
    let unknown = bits & !((1u32 << names.len()) - 1);
    if unknown != 0 {
        out.push(format!("0x{unknown:x}"));
    }
    out
}

/// Names of the set `window_flags` bits.
pub fn window_flag_names(flags: u16) -> Vec<String> {
    flag_names(flags as u32, &WINDOW_FLAG_NAMES)
}

/// Decoded Hello.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HelloView {
    /// Board id, 16 hex digits.
    pub board_id: String,
    /// Firmware `major.minor.patch`.
    pub firmware: String,
    /// Capability bits.
    pub capabilities: u32,
}

/// Decoded Status.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusView {
    /// Board monotonic clock.
    pub monotonic_us: u64,
    /// Board RTC, −1 if invalid.
    pub rtc_unix_ms: i64,
    /// Current run.
    pub run_id: u32,
    /// State name.
    pub state: String,
    /// Nonzero when a fault is latched.
    pub fault_code: u8,
    /// Configured pulse length.
    pub pulse_duration_ms: u16,
    /// Cooldown remaining.
    pub cooldown_remaining_ms: u32,
}

/// `FieldStatus` (0x18, 52 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FieldStatus {
    pub monotonic_us: u64,
    pub rtc_unix_ms: i64,
    pub next_pulse_unix_s: u32,
    pub schedule_flags: u8,
    pub windows_pending: u8,
    pub windows_dropped: u16,
    pub oldest_pending_run_id: u32,
    pub last_run_id: u32,
    pub calr_ppb: i32,
    pub last_rtc_step_ms: i32,
    pub boot_count: u16,
    pub v24_raw: i16,
    pub flash_used_kib: u32,
    pub schedule_crc: u16,
}

impl FieldStatus {
    /// Names of the set `schedule_flags` bits.
    pub fn flag_names(&self) -> Vec<String> {
        flag_names(self.schedule_flags as u32, &SCHEDULE_FLAG_NAMES)
    }

}

macro_rules! mirror {
    ($local:ident, $proto:ty, [$($f:ident),*]) => {
        impl From<$proto> for $local {
            fn from(m: $proto) -> Self {
                $local { $($f: m.$f),* }
            }
        }
        impl From<$local> for $proto {
            fn from(m: $local) -> Self {
                Self { $($f: m.$f),* }
            }
        }
    };
}

mirror!(FieldStatus, sap_flux_protocol::FieldStatus, [
    monotonic_us, rtc_unix_ms, next_pulse_unix_s, schedule_flags, windows_pending, windows_dropped,
    oldest_pending_run_id, last_run_id, calr_ppb, last_rtc_step_ms, boot_count, v24_raw,
    flash_used_kib, schedule_crc
]);

mirror!(WindowInfo, sap_flux_protocol::WindowInfo, [
    run_id, window_flags, channels, bin_ms_div10, n_bins, chunk_count, start_monotonic_us,
    start_rtc_unix_ms, end_monotonic_us, end_rtc_unix_ms, heater_on_offset_ms, pulse_duration_ms,
    sample_hz, v24_mean_raw, frames_total, frames_missing, data_crc32, base_raw
]);

/// `WindowInfo` (0x16, 90 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct WindowInfo {
    pub run_id: u32,
    pub window_flags: u16,
    pub channels: u8,
    pub bin_ms_div10: u8,
    pub n_bins: u16,
    pub chunk_count: u16,
    pub start_monotonic_us: u64,
    pub start_rtc_unix_ms: i64,
    pub end_monotonic_us: u64,
    pub end_rtc_unix_ms: i64,
    pub heater_on_offset_ms: u32,
    pub pulse_duration_ms: u16,
    pub sample_hz: u16,
    pub v24_mean_raw: i16,
    pub frames_total: u16,
    pub frames_missing: u16,
    pub data_crc32: u32,
    pub base_raw: [i16; 13],
}

impl WindowInfo {
    /// Bin width in ms.
    pub fn bin_ms(&self) -> u32 {
        self.bin_ms_div10 as u32 * 10
    }

    /// Expected window data length in bytes.
    pub fn data_len(&self) -> usize {
        self.n_bins as usize * self.channels as usize * 2
    }

}

/// One decoded board message.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BoardMsg {
    Hello(HelloView),
    Status(StatusView),
    FieldStatus(FieldStatus),
    WindowInfo(WindowInfo),
    /// A valid frame of a type the server doesn't interpret.
    Other { message_type: u8, payload_hex: String },
}

/// A decoded frame with its board sequence number.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoardFrame {
    pub sequence: u32,
    #[serde(flatten)]
    pub msg: BoardMsg,
}

/// Frame decode errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Truncated,
    Crc,
    Version(u8),
    PayloadLength { message_type: u8, len: usize },
    Protocol(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Truncated => f.write_str("truncated frame"),
            FrameError::Crc => f.write_str("frame CRC mismatch"),
            FrameError::Version(v) => write!(f, "frame version {v}"),
            FrameError::PayloadLength { message_type, len } => {
                write!(f, "type 0x{message_type:02x}: bad payload length {len}")
            }
            FrameError::Protocol(e) => write!(f, "protocol: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Length of the frame at the start of `bytes`, from its header.
fn frame_len(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < HEADER_LEN {
        return None;
    }
    Some(HEADER_LEN + u16::from_le_bytes([bytes[2], bytes[3]]) as usize + CRC_LEN)
}

/// Decode exactly one frame (`bytes` must be the whole frame).
pub fn parse_frame(bytes: &[u8]) -> Result<BoardFrame, FrameError> {
    let len = frame_len(bytes).ok_or(FrameError::Truncated)?;
    if len != bytes.len() {
        return Err(FrameError::Truncated);
    }
    let crc_at = len - CRC_LEN;
    let crc = u32::from_le_bytes(bytes[crc_at..].try_into().unwrap());
    if crc != crc32(&bytes[..crc_at]) {
        return Err(FrameError::Crc);
    }
    let message_type = bytes[1];
    let frame = match decode_frame(bytes) {
        Ok(f) => f,
        Err(DecodeError::UnknownMessageType(_)) => {
            return Ok(BoardFrame {
                sequence: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                msg: BoardMsg::Other {
                    message_type,
                    payload_hex: hex(&bytes[HEADER_LEN..crc_at]),
                },
            })
        }
        Err(DecodeError::Version(v)) => return Err(FrameError::Version(v)),
        Err(DecodeError::PayloadLength { actual, .. }) => {
            return Err(FrameError::PayloadLength {
                message_type,
                len: actual,
            })
        }
        Err(e) => return Err(FrameError::Protocol(format!("{e:?}"))),
    };
    let msg = match frame.message {
        Message::Hello(h) => BoardMsg::Hello(HelloView {
            board_id: format!("{:016X}", h.board_id),
            firmware: format!(
                "{}.{}.{}",
                (h.firmware_version >> 16) & 0xFF,
                (h.firmware_version >> 8) & 0xFF,
                h.firmware_version & 0xFF
            ),
            capabilities: h.capabilities,
        }),
        Message::Status(s) => BoardMsg::Status(StatusView {
            monotonic_us: s.monotonic_us,
            rtc_unix_ms: s.rtc_unix_ms,
            run_id: s.run_id,
            state: STATE_NAMES
                .get(s.state as usize)
                .copied()
                .unwrap_or("?")
                .to_string(),
            fault_code: s.fault_code,
            pulse_duration_ms: s.pulse_duration_ms,
            cooldown_remaining_ms: s.cooldown_remaining_ms,
        }),
        Message::FieldStatus(f) => BoardMsg::FieldStatus(f.into()),
        Message::WindowInfo(w) => BoardMsg::WindowInfo(w.into()),
        _ => BoardMsg::Other {
            message_type,
            payload_hex: hex(&bytes[HEADER_LEN..crc_at]),
        },
    };
    Ok(BoardFrame {
        sequence: frame.sequence,
        msg,
    })
}

/// Split concatenated decoded frames and decode each. A frame whose header
/// claims more bytes than remain ends the walk with `Truncated`.
pub fn parse_frames(mut bytes: &[u8]) -> Vec<Result<BoardFrame, FrameError>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        match frame_len(bytes) {
            Some(n) if n <= bytes.len() => {
                out.push(parse_frame(&bytes[..n]));
                bytes = &bytes[n..];
            }
            _ => {
                out.push(Err(FrameError::Truncated));
                break;
            }
        }
    }
    out
}

/// Build a decoded v1 frame (header + payload + CRC-32).
pub fn build_frame(message_type: u8, sequence: u32, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(HEADER_LEN + payload.len() + CRC_LEN);
    f.push(sap_flux_protocol::PROTOCOL_VERSION);
    f.push(message_type);
    f.extend((payload.len() as u16).to_le_bytes());
    f.extend(sequence.to_le_bytes());
    f.extend_from_slice(payload);
    let crc = crc32(&f);
    f.extend(crc.to_le_bytes());
    f
}

/// Build a decoded frame (no COBS) for a protocol-crate message.
pub fn build_crate_frame(msg: &Message, sequence: u32) -> Vec<u8> {
    let mut out = [0u8; sap_flux_protocol::MAX_FRAME_LEN];
    let n = sap_flux_protocol::encode_frame(msg, sequence, &mut out).expect("buffer is max size");
    out[..n].to_vec()
}

/// Decoded FieldStatus frame.
pub fn field_status_frame(fs: &FieldStatus, sequence: u32) -> Vec<u8> {
    build_crate_frame(&Message::FieldStatus((*fs).into()), sequence)
}

/// Decoded WindowInfo frame.
pub fn window_info_frame(wi: &WindowInfo, sequence: u32) -> Vec<u8> {
    build_crate_frame(&Message::WindowInfo((*wi).into()), sequence)
}

/// Lowercase hex.
pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// Parse hex (whitespace and `0x` prefix tolerated).
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s: String = s
        .trim()
        .trim_start_matches("0x")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sap_flux_protocol::{BoardState, Hello, Status};

    #[test]
    fn hello_status_via_crate() {
        let hello = build_crate_frame(
            &Message::Hello(Hello {
                board_id: 0xD15C_55B4_116E_105E,
                firmware_version: 0x00_01_1F, // 0.1.31
                capabilities: 15,
            }),
            3,
        );
        // Own frame builder must agree with the crate encoder byte for byte.
        let mut payload = 0xD15C_55B4_116E_105Eu64.to_le_bytes().to_vec();
        payload.extend(((1u32 << 8) | 31).to_le_bytes());
        payload.extend(15u32.to_le_bytes());
        assert_eq!(build_frame(MSG_HELLO, 3, &payload), hello);

        let status = build_crate_frame(
            &Message::Status(Status {
                monotonic_us: 5,
                rtc_unix_ms: -1,
                run_id: 0,
                state: BoardState::Cooldown,
                fault_code: 0,
                pulse_duration_ms: 1000,
                cooldown_remaining_ms: 12,
            }),
            4,
        );
        let fs = FieldStatus {
            schedule_crc: 0x4612,
            schedule_flags: 0b1001,
            ..Default::default()
        };
        let fsf = field_status_frame(&fs, 5);
        let all: Vec<u8> = [hello, status, fsf].concat();
        let frames: Vec<_> = parse_frames(&all).into_iter().map(Result::unwrap).collect();
        assert_eq!(frames.len(), 3);
        match &frames[0].msg {
            BoardMsg::Hello(h) => {
                assert_eq!(h.board_id, "D15C55B4116E105E");
                assert_eq!(h.firmware, "0.1.31");
            }
            m => panic!("{m:?}"),
        }
        match &frames[1].msg {
            BoardMsg::Status(s) => assert_eq!(s.state, "Cooldown"),
            m => panic!("{m:?}"),
        }
        match &frames[2].msg {
            BoardMsg::FieldStatus(f) => {
                assert_eq!(*f, fs);
                assert_eq!(f.flag_names(), ["enabled", "rtc_valid"]);
            }
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn window_info_roundtrip_and_corruption() {
        let mut wi = WindowInfo {
            run_id: 7,
            channels: 13,
            bin_ms_div10: 50,
            n_bins: 482,
            ..Default::default()
        };
        wi.base_raw[12] = -5;
        let f = window_info_frame(&wi, 1);
        assert_eq!(f.len(), WINDOW_INFO_FRAME_LEN);
        assert_eq!(parse_frame(&f).unwrap().msg, BoardMsg::WindowInfo(wi));
        let mut bad = f.clone();
        bad[20] ^= 1;
        assert_eq!(parse_frame(&bad), Err(FrameError::Crc));
        assert_eq!(parse_frames(&f[..50]), vec![Err(FrameError::Truncated)]);
    }
}
