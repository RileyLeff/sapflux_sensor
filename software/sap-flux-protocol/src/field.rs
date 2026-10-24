//! Field-mode helpers: schedule bytes and CRC, schedule limits, window data
//! chunk math, and the binned `i16` value encoding.
//!
//! # Window data
//!
//! A stored pulse window is described by a [`WindowInfo`] header followed by
//! `n_bins` bins of `channels` (13) little-endian `i16` values each, in the
//! order probe-1 T1..T6, probe-2 T1..T6, excitation. Bin `k` covers
//! `[start + k·bin_ms, start + (k+1)·bin_ms)` on the monotonic clock.
//!
//! Each value is `clamp(round(16 × (bin_mean_raw − base_raw[c])))`, i.e. the
//! bin mean relative to the channel base in 1/16-count units
//! ([`BIN_VALUE_SCALE`]). A bin with no valid frames for a channel stores
//! [`BIN_VALUE_MISSING`] (`−32768`). Valid values are clamped to
//! `−32767..=32767` so they can never collide with the sentinel. Rounding is
//! half away from zero, computed exactly in integers by
//! [`encode_bin_value`].
//!
//! `data_crc32` is the protocol CRC-32 ([`crate::crc`]) over the whole data
//! byte string. The data is carried in [`WINDOW_CHUNK_DATA_LEN`] (100) byte
//! chunks: chunk `i` holds bytes `[100·i, 100·i + len)`.

use crate::crc::crc32;
use crate::msg::{Schedule, WindowInfo};
use crate::{SCHEDULE_FLAG_REQUIRE_LEASE, WINDOW_CHANNELS, WINDOW_CHUNK_DATA_LEN};
use core::ops::Range;

/// Length of the schedule byte string (`enabled` through `bin_ms`).
pub const SCHEDULE_LEN: usize = 20;

/// Fixed-point scale of a window bin value: 16 steps per raw count.
pub const BIN_VALUE_SCALE: i32 = 16;

/// Window bin value meaning "no valid frames for this channel in this bin".
pub const BIN_VALUE_MISSING: i16 = i16::MIN;

/// Allowed `pulse_duration_ms` values.
pub const SCHEDULE_PULSE_DURATIONS_MS: [u16; 3] = [500, 1000, 1500];
/// Allowed `sample_hz` values.
pub const SCHEDULE_SAMPLE_HZ: [u16; 3] = [10, 20, 25];
/// Allowed `bin_ms` values.
pub const SCHEDULE_BIN_MS: [u16; 3] = [250, 500, 1000];
/// Most bins one window may hold (the board's RAM slot size).
pub const SCHEDULE_MAX_WINDOW_BINS: u32 = 1000;
/// Margin [`Schedule::validate`] adds to the nominal window span: a real
/// window runs a little long (recovery starts when the board sees the
/// pulse end, up to a frame late), and the RAM slot must still hold it.
pub const SCHEDULE_WINDOW_SLACK_MS: u32 = 1000;
/// Minimum `interval_s`.
pub const SCHEDULE_MIN_INTERVAL_S: u32 = 600;
/// Allowed `baseline_s` range.
pub const SCHEDULE_BASELINE_S: Range<u16> = 10..301;
/// Allowed `recovery_s` range.
pub const SCHEDULE_RECOVERY_S: Range<u16> = 60..601;

/// Why a [`Schedule`] fails [`Schedule::validate`]. The board answers any of
/// these with `Ack` result
/// [`ACK_INVALID_PARAMETER`](crate::ACK_INVALID_PARAMETER).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// `enabled` is neither 0 nor 1.
    Enabled,
    /// `flags` has bits other than `SCHEDULE_FLAG_*` set.
    Flags,
    /// `pulse_duration_ms` not in {500, 1000, 1500}.
    PulseDuration,
    /// `interval_s` < 600.
    Interval,
    /// `baseline_s` outside 10–300.
    Baseline,
    /// `recovery_s` outside 60–600.
    Recovery,
    /// `baseline_s + recovery_s + cooldown` ≥ `interval_s`.
    Timing,
    /// `sample_hz` not in {10, 20, 25}.
    SampleHz,
    /// `bin_ms` not in {250, 500, 1000}.
    BinMs,
    /// The window, plus [`SCHEDULE_WINDOW_SLACK_MS`], would need more than
    /// [`SCHEDULE_MAX_WINDOW_BINS`] bins.
    WindowTooLong,
}

impl Schedule {
    /// Factory default (blank EEPROM): disabled, 1000 ms every 3600 s,
    /// phase 0, baseline 60 s, recovery 180 s, 25 Hz, 500 ms bins.
    pub const FACTORY_DEFAULT: Schedule = Schedule {
        enabled: 0,
        flags: 0,
        pulse_duration_ms: 1000,
        interval_s: 3600,
        phase_s: 0,
        baseline_s: 60,
        recovery_s: 180,
        sample_hz: 25,
        bin_ms: 500,
    };

    /// Serialize to the 20 schedule bytes.
    pub fn to_bytes(&self) -> [u8; SCHEDULE_LEN] {
        let mut b = [0u8; SCHEDULE_LEN];
        b[0] = self.enabled;
        b[1] = self.flags;
        b[2..4].copy_from_slice(&self.pulse_duration_ms.to_le_bytes());
        b[4..8].copy_from_slice(&self.interval_s.to_le_bytes());
        b[8..12].copy_from_slice(&self.phase_s.to_le_bytes());
        b[12..14].copy_from_slice(&self.baseline_s.to_le_bytes());
        b[14..16].copy_from_slice(&self.recovery_s.to_le_bytes());
        b[16..18].copy_from_slice(&self.sample_hz.to_le_bytes());
        b[18..20].copy_from_slice(&self.bin_ms.to_le_bytes());
        b
    }

    /// Parse the 20 schedule bytes (e.g. `CheckInReply.schedule`). No
    /// validation; see [`validate`](Self::validate).
    pub fn from_bytes(b: &[u8; SCHEDULE_LEN]) -> Self {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Self {
            enabled: b[0],
            flags: b[1],
            pulse_duration_ms: u16_at(2),
            interval_s: u32_at(4),
            phase_s: u32_at(8),
            baseline_s: u16_at(12),
            recovery_s: u16_at(14),
            sample_hz: u16_at(16),
            bin_ms: u16_at(18),
        }
    }

    /// The `schedule_crc` reported in `FieldStatus` for this schedule.
    pub fn crc16(&self) -> u16 {
        schedule_crc(&self.to_bytes())
    }

    /// `enabled != 0`.
    pub fn is_enabled(&self) -> bool {
        self.enabled != 0
    }

    /// `REQUIRE_LEASE` flag set.
    pub fn requires_lease(&self) -> bool {
        self.flags & SCHEDULE_FLAG_REQUIRE_LEASE != 0
    }

    /// Check the `SetSchedule` limits. `cooldown_ms` is the board's
    /// mandatory post-pulse cooldown (600 000 ms in production firmware).
    ///
    /// Applies to disabled schedules too, so every stored schedule is
    /// runnable when enabled later.
    pub fn validate(&self, cooldown_ms: u32) -> Result<(), ScheduleError> {
        if self.enabled > 1 {
            return Err(ScheduleError::Enabled);
        }
        if self.flags & !SCHEDULE_FLAG_REQUIRE_LEASE != 0 {
            return Err(ScheduleError::Flags);
        }
        if !SCHEDULE_PULSE_DURATIONS_MS.contains(&self.pulse_duration_ms) {
            return Err(ScheduleError::PulseDuration);
        }
        if self.interval_s < SCHEDULE_MIN_INTERVAL_S {
            return Err(ScheduleError::Interval);
        }
        if !SCHEDULE_BASELINE_S.contains(&self.baseline_s) {
            return Err(ScheduleError::Baseline);
        }
        if !SCHEDULE_RECOVERY_S.contains(&self.recovery_s) {
            return Err(ScheduleError::Recovery);
        }
        let busy_ms = (self.baseline_s as u64 + self.recovery_s as u64) * 1000 + cooldown_ms as u64;
        if busy_ms >= self.interval_s as u64 * 1000 {
            return Err(ScheduleError::Timing);
        }
        if !SCHEDULE_SAMPLE_HZ.contains(&self.sample_hz) {
            return Err(ScheduleError::SampleHz);
        }
        if !SCHEDULE_BIN_MS.contains(&self.bin_ms) {
            return Err(ScheduleError::BinMs);
        }
        let bin = self.bin_ms.max(1) as u32;
        if (self.window_span_ms() + SCHEDULE_WINDOW_SLACK_MS).div_ceil(bin)
            > SCHEDULE_MAX_WINDOW_BINS
        {
            return Err(ScheduleError::WindowTooLong);
        }
        Ok(())
    }

    /// Bins in the window this schedule produces:
    /// `ceil((baseline + pulse + recovery) / bin_ms)`.
    pub fn window_bins(&self) -> u32 {
        let bin = self.bin_ms.max(1) as u32;
        self.window_span_ms().div_ceil(bin)
    }

    fn window_span_ms(&self) -> u32 {
        (self.baseline_s as u32 + self.recovery_s as u32) * 1000 + self.pulse_duration_ms as u32
    }
}

impl Default for Schedule {
    fn default() -> Self {
        Self::FACTORY_DEFAULT
    }
}

/// `schedule_crc`: low 16 bits of the protocol CRC-32 over schedule bytes.
pub fn schedule_crc(schedule_bytes: &[u8]) -> u16 {
    crc32(schedule_bytes) as u16
}

/// Window data length in bytes: `n_bins × channels × 2`.
pub fn window_data_len(n_bins: u16, channels: u8) -> usize {
    n_bins as usize * channels as usize * 2
}

/// Number of chunks for `data_len` bytes: `ceil(data_len / 100)`.
pub fn chunk_count(data_len: usize) -> usize {
    data_len.div_ceil(WINDOW_CHUNK_DATA_LEN)
}

/// Data byte range carried by `chunk`, or `None` past the end.
pub fn chunk_range(chunk: u16, data_len: usize) -> Option<Range<usize>> {
    let start = chunk as usize * WINDOW_CHUNK_DATA_LEN;
    if start >= data_len {
        return None;
    }
    Some(start..(start + WINDOW_CHUNK_DATA_LEN).min(data_len))
}

/// Byte offset of `(bin, channel)` in the window data.
pub fn bin_value_offset(bin: usize, channel: usize, channels: usize) -> usize {
    (bin * channels + channel) * 2
}

/// Read value `(bin, channel)` from window data, or `None` if out of range.
pub fn read_bin_value(data: &[u8], bin: usize, channel: usize, channels: usize) -> Option<i16> {
    if channel >= channels {
        return None;
    }
    let off = bin_value_offset(bin, channel, channels);
    let b = data.get(off..off + 2)?;
    Some(i16::from_le_bytes([b[0], b[1]]))
}

/// Integer division rounding half away from zero; `den` must be > 0.
fn div_round(num: i64, den: i64) -> i64 {
    if num >= 0 {
        (2 * num + den) / (2 * den)
    } else {
        -((-2 * num + den) / (2 * den))
    }
}

/// `round(sum / count)` (half away from zero) as an `i16`, e.g. a channel's
/// `base_raw` from its first-bin sum. `None` if `count == 0`.
pub fn mean_raw_rounded(sum: i32, count: u32) -> Option<i16> {
    if count == 0 {
        return None;
    }
    let m = div_round(sum as i64, count as i64);
    Some(m.clamp(i16::MIN as i64, i16::MAX as i64) as i16)
}

/// Encode one bin value from the sum and count of a channel's valid raw
/// readings in the bin: `clamp(round(16 × (sum/count − base_raw)),
/// −32767, 32767)`, exact in integers, rounding half away from zero.
/// `count == 0` gives [`BIN_VALUE_MISSING`].
pub fn encode_bin_value(sum: i32, count: u32, base_raw: i16) -> i16 {
    if count == 0 {
        return BIN_VALUE_MISSING;
    }
    let scale = BIN_VALUE_SCALE as i64;
    let num = scale * sum as i64 - scale * base_raw as i64 * count as i64;
    let v = div_round(num, count as i64);
    v.clamp(-(i16::MAX as i64), i16::MAX as i64) as i16
}

/// Decode a bin value to `16 × raw` (exact, in 1/16-count units).
/// `None` for [`BIN_VALUE_MISSING`].
pub fn decode_bin_value_x16(value: i16, base_raw: i16) -> Option<i32> {
    if value == BIN_VALUE_MISSING {
        return None;
    }
    Some(base_raw as i32 * BIN_VALUE_SCALE + value as i32)
}

/// Decode a bin value back to (fractional) raw counts.
/// `None` for [`BIN_VALUE_MISSING`].
pub fn decode_bin_value(value: i16, base_raw: i16) -> Option<f32> {
    decode_bin_value_x16(value, base_raw).map(|x| x as f32 / BIN_VALUE_SCALE as f32)
}

impl WindowInfo {
    /// Bin width in ms (`bin_ms_div10 × 10`).
    pub fn bin_ms(&self) -> u32 {
        self.bin_ms_div10 as u32 * 10
    }

    /// Window data length in bytes.
    pub fn data_len(&self) -> usize {
        window_data_len(self.n_bins, self.channels)
    }

    /// `ceil(data_len / 100)`; equals `chunk_count` on a consistent header.
    pub fn expected_chunk_count(&self) -> usize {
        chunk_count(self.data_len())
    }

    /// Monotonic start time of bin `k`.
    pub fn bin_start_monotonic_us(&self, k: u32) -> u64 {
        self.start_monotonic_us + k as u64 * self.bin_ms() as u64 * 1000
    }

    /// Whether the header is self-consistent (13 channels, nonzero bin
    /// width, matching chunk count).
    pub fn is_consistent(&self) -> bool {
        self.channels as usize == WINDOW_CHANNELS
            && self.bin_ms_div10 != 0
            && self.chunk_count as usize == self.expected_chunk_count()
    }

    /// Whether `data` has the expected length and matches `data_crc32`.
    pub fn verify_data(&self, data: &[u8]) -> bool {
        data.len() == self.data_len() && crc32(data) == self.data_crc32
    }
}
