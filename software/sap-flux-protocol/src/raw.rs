//! Raw 25 Hz frame blocks: the board's flash log format, also the byte
//! stream `GetRaw` / `RawChunk` return (`docs/field-protocol.md` §1).
//!
//! A block is one 256-byte flash page:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 2 | `magic` = `0x4652` (bytes `"RF"`) |
//! | 2 | 1 | `version` = 1 |
//! | 3 | 1 | `n_frames` |
//! | 4 | 4 | `run_id` |
//! | 8 | 4 | `seq` (board-wide block counter, ring order) |
//! | 12 | 4 | `block_in_run` (0 = first block of the run) |
//! | 16 | 8 | `first_monotonic_us` (frame 0) |
//! | 24 | 8 | `first_rtc_unix_ms` (frame 0, −1 = RTC invalid) |
//! | 32 | 220 | frames, then `0xFF` padding |
//! | 252 | 4 | `crc32` (protocol CRC over bytes 0..252) |
//!
//! Each frame holds 14 values: thermistors probe 1 T1..T6, probe 2 T1..T6,
//! excitation, v24 (raw counts, `i16::MIN` = failed read). Frame encodings:
//!
//! * absolute, 33 B: `0x00`, `dt_us: u16`, `flags: u8` (Sample12 flags,
//!   low byte), `state: u8`, `values: [i16; 14]`;
//! * delta, 17 B: `0x01`, `dt_us: u16`, `delta: [i8; 14]` (value − the
//!   previous frame's value; flags and state unchanged).
//!
//! `dt_us` is the monotonic step from the previous frame (0 for frame 0).
//! Frame 0 of every block is absolute, so each block decodes on its own.
//! A frame's RTC time is `first_rtc_unix_ms + round((monotonic_us −
//! first_monotonic_us) / 1000)`, or −1 when `first_rtc_unix_ms` is −1 (the
//! board never steps the RTC inside a run).

use crate::crc::crc32;

/// Bytes per block (one flash page).
pub const BLOCK_LEN: usize = 256;
/// Fixed header bytes before the frames.
pub const HEADER_LEN: usize = 32;
/// Bytes a boot scan reads per sector: everything up to `first_monotonic_us`.
pub const PREFIX_LEN: usize = 16;
/// Offset of the block CRC-32.
pub const CRC_OFFSET: usize = BLOCK_LEN - 4;
/// Frame bytes available per block.
pub const PAYLOAD_LEN: usize = CRC_OFFSET - HEADER_LEN;
/// Block magic (little-endian bytes `"RF"`).
pub const MAGIC: u16 = 0x4652;
/// Block format version.
pub const VERSION: u8 = 1;
/// Values per frame: 12 thermistors, excitation, v24.
pub const VALUES: usize = 14;
/// Raw value of a failed read.
pub const INVALID: i16 = i16::MIN;
const TAG_ABSOLUTE: u8 = 0x00;
const TAG_DELTA: u8 = 0x01;
/// Size of an absolute frame.
pub const ABSOLUTE_LEN: usize = 5 + 2 * VALUES;
/// Size of a delta frame.
pub const DELTA_LEN: usize = 3 + VALUES;
/// Most frames one block can hold (one absolute, the rest deltas).
pub const MAX_FRAMES: usize = 1 + (PAYLOAD_LEN - ABSOLUTE_LEN) / DELTA_LEN;

/// One logged frame, the content of a `Sample12` without `run_id` (which
/// the block carries).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawFrame {
    /// Microseconds since boot.
    pub monotonic_us: u64,
    /// RTC Unix ms, −1 when invalid.
    pub rtc_unix_ms: i64,
    /// Low byte of the `Sample12` flags.
    pub flags: u8,
    /// `BoardState` as `u8`.
    pub state: u8,
    /// P1 T1..T6, P2 T1..T6, excitation, v24 (raw counts).
    pub values: [i16; VALUES],
}

/// The fixed fields a sector scan needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefix {
    /// Frames in the block (1..=12).
    pub n_frames: u8,
    /// Run the frames belong to.
    pub run_id: u32,
    /// Board-wide block counter.
    pub seq: u32,
    /// Index of this block within its run.
    pub block_in_run: u32,
}

/// A decoded block header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    /// Ring fields.
    pub prefix: Prefix,
    /// Monotonic time of frame 0.
    pub first_monotonic_us: u64,
    /// RTC time of frame 0 (−1 = invalid).
    pub first_rtc_unix_ms: i64,
}

/// Why a block did not decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawError {
    /// Wrong magic or version (blank or foreign page).
    NotABlock,
    /// CRC mismatch (torn or corrupted page).
    Crc,
    /// CRC good but the frames do not parse (encoder bug).
    Malformed,
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(a)
}

/// Parse the first [`PREFIX_LEN`] bytes of a page; `None` unless the magic,
/// version and frame count look like a block.
pub fn parse_prefix(b: &[u8]) -> Option<Prefix> {
    if b.len() < PREFIX_LEN
        || u16::from_le_bytes([b[0], b[1]]) != MAGIC
        || b[2] != VERSION
        || b[3] == 0
        || b[3] as usize > MAX_FRAMES
    {
        return None;
    }
    Some(Prefix {
        n_frames: b[3],
        run_id: u32_at(b, 4),
        seq: u32_at(b, 8),
        block_in_run: u32_at(b, 12),
    })
}

/// Header of a block whose magic, version and CRC are good.
pub fn verify_block(b: &[u8; BLOCK_LEN]) -> Result<BlockHeader, RawError> {
    let prefix = parse_prefix(b).ok_or(RawError::NotABlock)?;
    if crc32(&b[..CRC_OFFSET]) != u32_at(b, CRC_OFFSET) {
        return Err(RawError::Crc);
    }
    Ok(BlockHeader {
        prefix,
        first_monotonic_us: u64_at(b, 16),
        first_rtc_unix_ms: u64_at(b, 24) as i64,
    })
}

/// Fill in the ring fields of a finished block and seal it with its CRC.
pub fn seal_block(b: &mut [u8; BLOCK_LEN], seq: u32, block_in_run: u32) {
    b[8..12].copy_from_slice(&seq.to_le_bytes());
    b[12..16].copy_from_slice(&block_in_run.to_le_bytes());
    let crc = crc32(&b[..CRC_OFFSET]);
    b[CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
}

/// Run id stored in a (possibly unsealed) block.
pub fn block_run_id(b: &[u8; BLOCK_LEN]) -> u32 {
    u32_at(b, 4)
}

/// Verify and decode a block, calling `out` for each frame in order.
pub fn decode_block(
    b: &[u8; BLOCK_LEN],
    mut out: impl FnMut(RawFrame),
) -> Result<BlockHeader, RawError> {
    let header = verify_block(b)?;
    let mut pos = HEADER_LEN;
    let mut prev: Option<RawFrame> = None;
    for _ in 0..header.prefix.n_frames {
        let tag = *b.get(pos).ok_or(RawError::Malformed)?;
        let len = match tag {
            TAG_ABSOLUTE => ABSOLUTE_LEN,
            TAG_DELTA => DELTA_LEN,
            _ => return Err(RawError::Malformed),
        };
        if pos + len > CRC_OFFSET {
            return Err(RawError::Malformed);
        }
        let f = &b[pos..pos + len];
        let dt = u16::from_le_bytes([f[1], f[2]]) as u64;
        let frame = match (tag, prev) {
            (TAG_ABSOLUTE, _) => {
                let base = prev.map_or(header.first_monotonic_us, |p| p.monotonic_us + dt);
                let mut values = [0i16; VALUES];
                for (i, v) in values.iter_mut().enumerate() {
                    *v = i16::from_le_bytes([f[5 + 2 * i], f[6 + 2 * i]]);
                }
                RawFrame {
                    monotonic_us: base,
                    rtc_unix_ms: 0,
                    flags: f[3],
                    state: f[4],
                    values,
                }
            }
            (_, Some(p)) => {
                let mut values = p.values;
                for (i, v) in values.iter_mut().enumerate() {
                    *v = v.wrapping_add(f[3 + i] as i8 as i16);
                }
                RawFrame {
                    monotonic_us: p.monotonic_us + dt,
                    values,
                    ..p
                }
            }
            // A delta cannot open a block.
            _ => return Err(RawError::Malformed),
        };
        let frame = RawFrame {
            rtc_unix_ms: rtc_at(&header, frame.monotonic_us),
            ..frame
        };
        out(frame);
        prev = Some(frame);
        pos += len;
    }
    Ok(header)
}

fn rtc_at(h: &BlockHeader, monotonic_us: u64) -> i64 {
    if h.first_rtc_unix_ms < 0 {
        return -1;
    }
    let dt_us = monotonic_us.saturating_sub(h.first_monotonic_us);
    h.first_rtc_unix_ms + ((dt_us + 500) / 1000) as i64
}

fn delta_fits(prev: &RawFrame, frame: &RawFrame) -> bool {
    prev.flags == frame.flags
        && prev.state == frame.state
        && prev.values.iter().zip(&frame.values).all(|(&p, &v)| {
            p != INVALID && v != INVALID && (-127..=127).contains(&(v as i32 - p as i32))
        })
}

/// Packs frames into blocks. Blocks come out unsealed (`seq`,
/// `block_in_run` and CRC are filled in by [`seal_block`] when written).
pub struct BlockBuilder {
    buf: [u8; BLOCK_LEN],
    /// Frame bytes used (the next frame goes at `HEADER_LEN + used`).
    used: usize,
    n: u8,
    prev: RawFrame,
    first_rtc_valid: bool,
}

const ZERO_FRAME: RawFrame = RawFrame {
    monotonic_us: 0,
    rtc_unix_ms: 0,
    flags: 0,
    state: 0,
    values: [0; VALUES],
};

impl Default for BlockBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockBuilder {
    /// An empty builder (all-zero, so a `static` holding one stays in
    /// `.bss`).
    pub const fn new() -> Self {
        Self {
            buf: [0; BLOCK_LEN],
            used: 0,
            n: 0,
            prev: ZERO_FRAME,
            first_rtc_valid: false,
        }
    }

    /// No frames buffered.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Run id of the block being built (meaningless when empty).
    pub fn run_id(&self) -> u32 {
        block_run_id(&self.buf)
    }

    /// Add a frame of `run_id`. Returns the previous block when this frame
    /// had to start a new one: other run, payload full, a monotonic step
    /// outside `0..=65535` µs, or a change of RTC validity.
    pub fn push(&mut self, run_id: u32, frame: &RawFrame) -> Option<[u8; BLOCK_LEN]> {
        if self.n == 0 {
            self.start(run_id, frame);
            return None;
        }
        let dt = frame.monotonic_us.checked_sub(self.prev.monotonic_us);
        let same_block = run_id == self.run_id()
            && (frame.rtc_unix_ms >= 0) == self.first_rtc_valid
            && dt.is_some_and(|d| d <= u16::MAX as u64);
        let delta = delta_fits(&self.prev, frame);
        let len = if delta { DELTA_LEN } else { ABSOLUTE_LEN };
        if !same_block || HEADER_LEN + self.used + len > CRC_OFFSET || self.n as usize >= MAX_FRAMES
        {
            let done = self.finish();
            self.start(run_id, frame);
            return done;
        }
        self.put(frame, dt.unwrap_or(0) as u16, delta);
        None
    }

    /// Close the block being built, if any.
    pub fn finish(&mut self) -> Option<[u8; BLOCK_LEN]> {
        if self.n == 0 {
            return None;
        }
        self.buf[3] = self.n;
        self.buf[HEADER_LEN + self.used..CRC_OFFSET].fill(0xFF);
        let done = self.buf;
        *self = Self::new();
        Some(done)
    }

    fn start(&mut self, run_id: u32, frame: &RawFrame) {
        *self = Self::new();
        self.buf[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        self.buf[2] = VERSION;
        self.buf[4..8].copy_from_slice(&run_id.to_le_bytes());
        self.buf[16..24].copy_from_slice(&frame.monotonic_us.to_le_bytes());
        let rtc = if frame.rtc_unix_ms >= 0 {
            frame.rtc_unix_ms
        } else {
            -1
        };
        self.buf[24..32].copy_from_slice(&rtc.to_le_bytes());
        self.first_rtc_valid = frame.rtc_unix_ms >= 0;
        self.put(frame, 0, false);
    }

    fn put(&mut self, frame: &RawFrame, dt: u16, delta: bool) {
        let p = HEADER_LEN + self.used;
        let dt = dt.to_le_bytes();
        if delta {
            self.buf[p] = TAG_DELTA;
            self.buf[p + 1..p + 3].copy_from_slice(&dt);
            for i in 0..VALUES {
                self.buf[p + 3 + i] =
                    (frame.values[i] as i32 - self.prev.values[i] as i32) as i8 as u8;
            }
            self.used += DELTA_LEN;
        } else {
            self.buf[p] = TAG_ABSOLUTE;
            self.buf[p + 1..p + 3].copy_from_slice(&dt);
            self.buf[p + 3] = frame.flags;
            self.buf[p + 4] = frame.state;
            for i in 0..VALUES {
                self.buf[p + 5 + 2 * i..p + 7 + 2 * i]
                    .copy_from_slice(&frame.values[i].to_le_bytes());
            }
            self.used += ABSOLUTE_LEN;
        }
        self.n += 1;
        self.prev = *frame;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn frame(i: u64, v: i16) -> RawFrame {
        RawFrame {
            monotonic_us: 1_000_000 + i * 40_000,
            rtc_unix_ms: 1_793_000_000_000 + (i * 40) as i64,
            flags: 0x0e,
            state: 1,
            values: core::array::from_fn(|c| v + 100 * c as i16),
        }
    }

    fn roundtrip(frames: &[RawFrame]) -> (Vec<[u8; BLOCK_LEN]>, Vec<RawFrame>) {
        let mut b = BlockBuilder::new();
        let mut blocks = Vec::new();
        for f in frames {
            blocks.extend(b.push(7, f));
        }
        blocks.extend(b.finish());
        let mut out = Vec::new();
        for (k, blk) in blocks.iter_mut().enumerate() {
            seal_block(blk, 100 + k as u32, k as u32);
            let h = decode_block(blk, |f| out.push(f)).unwrap();
            assert_eq!(h.prefix.run_id, 7);
            assert_eq!(h.prefix.block_in_run, k as u32);
        }
        (blocks, out)
    }

    #[test]
    fn layout_constants() {
        assert_eq!(PAYLOAD_LEN, 220);
        assert_eq!(ABSOLUTE_LEN, 33);
        assert_eq!(DELTA_LEN, 17);
        assert_eq!(MAX_FRAMES, 12);
    }

    #[test]
    fn slow_signals_pack_twelve_frames_per_block_losslessly() {
        let frames: Vec<RawFrame> = (0..100)
            .map(|i| frame(i, 1000 + (i % 7) as i16 * 3))
            .collect();
        let (blocks, out) = roundtrip(&frames);
        assert_eq!(blocks.len(), 9); // ceil(100 / 12)
        assert_eq!(out, frames);
        assert_eq!(blocks[0][3], 12);
        // The last block is padded with 0xFF.
        let last = blocks.last().unwrap();
        assert_eq!(last[3], 4);
        assert!(last[HEADER_LEN + ABSOLUTE_LEN + 3 * DELTA_LEN..CRC_OFFSET]
            .iter()
            .all(|&b| b == 0xFF));
    }

    #[test]
    fn big_steps_invalid_reads_and_flag_changes_go_absolute() {
        let mut frames: Vec<RawFrame> = (0..12).map(|i| frame(i, 500)).collect();
        frames[3].values[2] = INVALID;
        frames[5].values[0] += 300;
        frames[7].flags |= 1; // heater on
        frames[9].state = 3;
        frames[10].values[13] = i16::MAX;
        let (_, out) = roundtrip(&frames);
        assert_eq!(out, frames);
    }

    #[test]
    fn gaps_runs_and_rtc_validity_split_blocks() {
        let mut frames: Vec<RawFrame> = (0..6).map(|i| frame(i, 10)).collect();
        // 70 ms gap: too long for a u16 µs step.
        frames[3].monotonic_us += 70_000;
        frames[4].monotonic_us += 70_000;
        frames[5].monotonic_us += 70_000;
        let (blocks, out) = roundtrip(&frames);
        assert_eq!(blocks.len(), 2);
        assert_eq!(out, frames);

        let mut b = BlockBuilder::new();
        assert!(b.push(1, &frame(0, 0)).is_none());
        let done = b.push(2, &frame(1, 0)).unwrap();
        assert_eq!(block_run_id(&done), 1);
        let mut f = frame(2, 0);
        f.rtc_unix_ms = -1;
        assert!(b.push(2, &f).is_some());
        let mut last = b.finish().unwrap();
        seal_block(&mut last, 0, 0);
        let mut got = Vec::new();
        decode_block(&last, |f| got.push(f)).unwrap();
        assert_eq!(got, [f]);
    }

    #[test]
    fn rtc_is_reconstructed_from_the_anchor() {
        let mut frames: Vec<RawFrame> = (0..3).map(|i| frame(i, 1)).collect();
        frames[1].monotonic_us += 499; // rounds down
        frames[1].rtc_unix_ms = frames[0].rtc_unix_ms + 40;
        frames[2].monotonic_us += 500; // rounds up
        frames[2].rtc_unix_ms = frames[0].rtc_unix_ms + 81;
        let (_, out) = roundtrip(&frames);
        assert_eq!(out, frames);
    }

    #[test]
    fn corruption_and_blank_pages_are_rejected() {
        let (mut blocks, _) = roundtrip(&[frame(0, 1)]);
        let blk = &mut blocks[0];
        assert!(verify_block(blk).is_ok());
        blk[40] ^= 1;
        assert_eq!(verify_block(blk), Err(RawError::Crc));
        assert_eq!(verify_block(&[0xFF; BLOCK_LEN]), Err(RawError::NotABlock));
        assert_eq!(parse_prefix(&[0xFF; 16]), None);
        assert_eq!(parse_prefix(&[0; 16]), None);
    }
}
