//! Frame encoding and the streaming decoder.

use crate::cobs;
use crate::crc::crc32;
use crate::msg::Message;
use crate::{CRC_LEN, HEADER_LEN, MAX_ENCODED_LEN, MAX_FRAME_LEN, MIN_FRAME_LEN, PROTOCOL_VERSION};

/// A fully decoded, validated frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Sender's frame sequence number.
    pub sequence: u32,
    /// The decoded message.
    pub message: Message,
}

/// Errors from [`encode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The output buffer is too small; provide at least
    /// [`MAX_ENCODED_LEN`] bytes.
    BufferTooSmall,
}

/// Errors from frame decoding. Each corrupt frame yields exactly one error
/// and the decoder resynchronizes at the next delimiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// COBS structure violated (embedded zero or truncated group).
    Cobs,
    /// Decoded frame shorter than header + CRC.
    TooShort {
        /// Actual decoded length.
        len: usize,
    },
    /// Frame exceeds [`MAX_FRAME_LEN`] (or its encoding exceeds the wire
    /// maximum).
    TooLong,
    /// Header `payload_len` disagrees with the actual decoded length.
    LengthMismatch {
        /// `payload_len` from the header.
        declared: u16,
        /// Actual payload bytes present.
        actual: usize,
    },
    /// CRC-32 check failed.
    Crc {
        /// CRC carried in the frame.
        received: u32,
        /// CRC computed over the frame contents.
        computed: u32,
    },
    /// Unsupported protocol version byte.
    Version(u8),
    /// `message_type` is not defined in protocol v1.
    UnknownMessageType(u8),
    /// Known message type but `payload_len` is wrong for it.
    PayloadLength {
        /// The message type.
        message_type: u8,
        /// Payload size required by that type.
        expected: usize,
        /// Payload size actually declared/present.
        actual: usize,
    },
    /// `state` byte is not a valid [`BoardState`](crate::BoardState).
    InvalidState(u8),
}

/// Encode `message` as a complete on-wire frame (COBS + trailing `0x00`
/// delimiter) into `out`, returning the number of bytes written.
///
/// `out` must be at least [`MAX_ENCODED_LEN`] bytes to hold any message.
pub fn encode(message: &Message, sequence: u32, out: &mut [u8]) -> Result<usize, EncodeError> {
    let payload_len = message.payload_len();
    let frame_len = HEADER_LEN + payload_len + CRC_LEN;
    debug_assert!(frame_len <= MAX_FRAME_LEN);

    let mut frame = [0u8; MAX_FRAME_LEN];
    frame[0] = PROTOCOL_VERSION;
    frame[1] = message.message_type();
    frame[2..4].copy_from_slice(&(payload_len as u16).to_le_bytes());
    frame[4..8].copy_from_slice(&sequence.to_le_bytes());
    message.write_payload(&mut frame[HEADER_LEN..HEADER_LEN + payload_len]);
    let crc = crc32(&frame[..HEADER_LEN + payload_len]);
    frame[HEADER_LEN + payload_len..frame_len].copy_from_slice(&crc.to_le_bytes());

    let encoded = cobs::encode(&frame[..frame_len], out).ok_or(EncodeError::BufferTooSmall)?;
    if encoded >= out.len() {
        return Err(EncodeError::BufferTooSmall);
    }
    out[encoded] = 0x00;
    Ok(encoded + 1)
}

/// Validate and parse one decoded (COBS-unstuffed) frame.
///
/// Most callers should use [`Decoder`] instead; this is exposed for tests
/// and for firmware that does its own buffering.
pub fn decode_frame(decoded: &[u8]) -> Result<Frame, DecodeError> {
    if decoded.len() < MIN_FRAME_LEN {
        return Err(DecodeError::TooShort { len: decoded.len() });
    }
    if decoded.len() > MAX_FRAME_LEN {
        return Err(DecodeError::TooLong);
    }
    let declared = u16::from_le_bytes([decoded[2], decoded[3]]);
    let actual_payload = decoded.len() - MIN_FRAME_LEN;
    if declared as usize != actual_payload {
        return Err(DecodeError::LengthMismatch {
            declared,
            actual: actual_payload,
        });
    }
    let crc_offset = decoded.len() - CRC_LEN;
    let received = u32::from_le_bytes([
        decoded[crc_offset],
        decoded[crc_offset + 1],
        decoded[crc_offset + 2],
        decoded[crc_offset + 3],
    ]);
    let computed = crc32(&decoded[..crc_offset]);
    if received != computed {
        return Err(DecodeError::Crc { received, computed });
    }
    if decoded[0] != PROTOCOL_VERSION {
        return Err(DecodeError::Version(decoded[0]));
    }
    let message_type = decoded[1];
    let sequence = u32::from_le_bytes([decoded[4], decoded[5], decoded[6], decoded[7]]);
    let message = Message::parse_payload(message_type, &decoded[HEADER_LEN..crc_offset])?;
    Ok(Frame { sequence, message })
}

/// Maximum number of stuffed (pre-delimiter) bytes a valid frame can occupy.
const MAX_STUFFED_LEN: usize = MAX_ENCODED_LEN - 1;

/// Push-style streaming frame decoder.
///
/// Feed raw serial bytes via [`push`](Self::push). Returns `Some(Ok(frame))`
/// when a delimiter completes a valid frame, `Some(Err(_))` when a delimiter
/// terminates corrupt input, and `None` otherwise. After any error the
/// decoder is already resynchronized: parsing resumes at the byte following
/// the delimiter. Fixed-size storage, no allocation, no panics.
pub struct Decoder {
    buf: [u8; MAX_STUFFED_LEN],
    len: usize,
    overflow: bool,
}

impl Decoder {
    /// Create an empty decoder.
    pub const fn new() -> Self {
        Self {
            buf: [0u8; MAX_STUFFED_LEN],
            len: 0,
            overflow: false,
        }
    }

    /// Discard any partially accumulated frame (e.g. on reconnect).
    pub fn reset(&mut self) {
        self.len = 0;
        self.overflow = false;
    }

    /// Feed one byte from the wire.
    pub fn push(&mut self, byte: u8) -> Option<Result<Frame, DecodeError>> {
        if byte != 0x00 {
            if self.len < MAX_STUFFED_LEN {
                self.buf[self.len] = byte;
                self.len += 1;
            } else {
                self.overflow = true;
            }
            return None;
        }
        let len = self.len;
        let overflowed = self.overflow;
        self.len = 0;
        self.overflow = false;
        if overflowed {
            return Some(Err(DecodeError::TooLong));
        }
        if len == 0 {
            // Idle delimiter (leading zero, or back-to-back delimiters used
            // for resynchronization). Not an error.
            return None;
        }
        let mut decoded = [0u8; MAX_FRAME_LEN];
        match cobs::decode(&self.buf[..len], &mut decoded) {
            Ok(n) => Some(decode_frame(&decoded[..n])),
            Err(cobs::CobsError::OutputOverflow) => Some(Err(DecodeError::TooLong)),
            Err(cobs::CobsError::Malformed) => Some(Err(DecodeError::Cobs)),
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}
