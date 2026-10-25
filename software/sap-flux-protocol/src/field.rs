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
//!
//! # SFK1 window data
//!
//! With [`WINDOW_FLAG_SFK1`] the data is instead one entry per probe whose
//! [`ProbeKind`] is set, in probe order: `probe: u8` (1 or 2), `len: u16`,
//! then `len` bytes of SFK1 record (the `sfk1` crate) holding that probe's
//! T1..T6, ratiometric against the excitation channel. `len = 0` means the
//! probe produced no record (no valid baseline frame). [`sfk1_records`]
//! walks the entries. `n_bins` is the data length in bytes, and
//! `bin_ms_div10` and `base_raw` are zero.
//!
//! The records' frame grid is the window's: frame `i` of every record is at
//! `start + i × 40 ms` on the monotonic clock (`sample_hz` 25), so record
//! time 0 (heater onset as the record sees it) is at
//! `start + n_baseline_bins × 5 s`, and the actual heater-on time is
//! `heater_on_offset_ms` after `start`.

use crate::crc::crc32;
use crate::msg::{Schedule, WindowInfo};
use crate::{
    SCHEDULE_FLAGS_DEFINED, SCHEDULE_FLAG_REQUIRE_LEASE, SCHEDULE_FLAG_SFK1, SCHEDULE_PROBE_SHIFT,
    WINDOW_CHANNELS, WINDOW_CHUNK_DATA_LEN, WINDOW_FLAG_SFK1,
};
use core::ops::Range;

/// Length of the schedule byte string (`enabled` through `sfk1_presets`).
pub const SCHEDULE_LEN: usize = 22;

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
/// The only `sample_hz` an SFK1 schedule may use (the East30 profiles'
/// 40 ms frame period).
pub const SCHEDULE_SFK1_SAMPLE_HZ: u16 = 25;
/// Bytes before each record in SFK1 window data: `probe: u8`, `len: u16`.
pub const SFK1_ENTRY_PREFIX_LEN: usize = 3;

/// What is installed on a probe port, from `SetSchedule.flags`
/// ([`SCHEDULE_PROBE_SHIFT`]). With the port's [`Sfk1Preset`] it picks the
/// SFK1 profile the board encodes that probe's thermistors with; binned
/// windows ignore both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum ProbeKind {
    /// Nothing, or not declared: no SFK1 record for this port.
    #[default]
    None = 0,
    /// East30 with both needles 6 mm from the heater (`sfk1::East30K12`
    /// and the other `sfk1::East30K*` presets).
    East30 = 1,
    /// East30 offset probe, needles 10 mm and 5 mm from the heater, either
    /// way up (`sfk1::East30Offset` and the `sfk1::East30OffsetK*` presets).
    East30Offset = 2,
}

impl ProbeKind {
    /// Parse a 2-bit code; `None` for the undefined code 3.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::East30),
            2 => Some(Self::East30Offset),
            _ => None,
        }
    }

    /// The 2-bit code.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// Spline knots of a port's SFK1 preset, which set the flows its records
/// reproduce accurately (`sfk1::BUNDLED` has each preset's range).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Sfk1Knots {
    /// 12 knots: to 70 cm/h.
    #[default]
    K12 = 0,
    /// 16 knots: to 120 cm/h (offset probe 110).
    K16 = 1,
    /// 20 knots: to 165 cm/h (offset probe 150).
    K20 = 2,
    /// 24 knots: to 200 cm/h.
    K24 = 3,
}

impl Sfk1Knots {
    /// The knot count.
    pub fn count(self) -> u8 {
        12 + 4 * self as u8
    }
}

/// Training prior of a port's SFK1 preset. It only changes record size:
/// every prior decodes the same curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Sfk1Prior {
    /// Smallest records when flow stays under about 32 cm/h.
    #[default]
    Slow = 0,
    /// Mostly slow flow with occasional fast spells.
    Mixed = 1,
    /// About the same size at any flow in the range.
    Fast = 2,
}

/// A port's SFK1 preset, one byte of `Schedule.sfk1_presets`: knots in bits
/// 0–1, prior in bits 2–3, the rest zero. Code 0 (12 knots, slow) is the
/// original `sfk1::East30K12` / `sfk1::East30Offset`. A board accepts only
/// the presets its firmware links (see its README) and answers others with
/// [`ACK_INVALID_PARAMETER`](crate::ACK_INVALID_PARAMETER).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sfk1Preset {
    /// Flow range.
    pub knots: Sfk1Knots,
    /// Record size trade-off.
    pub prior: Sfk1Prior,
}

impl Sfk1Preset {
    /// Parse a preset byte; `None` for an undefined code.
    pub fn from_code(code: u8) -> Option<Self> {
        let knots = match code & 0b11 {
            0 => Sfk1Knots::K12,
            1 => Sfk1Knots::K16,
            2 => Sfk1Knots::K20,
            _ => Sfk1Knots::K24,
        };
        let prior = match (code >> 2) & 0b11 {
            0 => Sfk1Prior::Slow,
            1 => Sfk1Prior::Mixed,
            2 => Sfk1Prior::Fast,
            _ => return None,
        };
        (code >> 4 == 0).then_some(Self { knots, prior })
    }

    /// The preset byte.
    pub fn code(self) -> u8 {
        self.knots as u8 | (self.prior as u8) << 2
    }
}

/// Text form `<knots>[-<prior>]`: `12`, `20-mixed`, `24-fast`. A slow prior
/// prints without a suffix.
impl core::fmt::Display for Sfk1Preset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.knots.count())?;
        match self.prior {
            Sfk1Prior::Slow => Ok(()),
            Sfk1Prior::Mixed => f.write_str("-mixed"),
            Sfk1Prior::Fast => f.write_str("-fast"),
        }
    }
}

/// A preset name that is not `<12|16|20|24>[-<slow|mixed|fast>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePresetError;

impl core::fmt::Display for ParsePresetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SFK1 preset must be 12, 16, 20 or 24 knots, optionally -slow, -mixed or -fast")
    }
}

impl core::error::Error for ParsePresetError {}

impl core::str::FromStr for Sfk1Preset {
    type Err = ParsePresetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (k, p) = s.split_once('-').unwrap_or((s, "slow"));
        let knots = match k {
            "12" => Sfk1Knots::K12,
            "16" => Sfk1Knots::K16,
            "20" => Sfk1Knots::K20,
            "24" => Sfk1Knots::K24,
            _ => return Err(ParsePresetError),
        };
        let prior = match p {
            "slow" => Sfk1Prior::Slow,
            "mixed" => Sfk1Prior::Mixed,
            "fast" => Sfk1Prior::Fast,
            _ => return Err(ParsePresetError),
        };
        Ok(Self { knots, prior })
    }
}

/// Margin [`Schedule::validate`] adds to the nominal window span: a real
/// window runs long when firing is retried (up to the board's 3 s fire
/// grace, which stretches the baseline) and because recovery starts when
/// the board sees the pulse end (up to a frame late; 1 s allowed). The RAM
/// slot must still hold it.
pub const SCHEDULE_WINDOW_SLACK_MS: u32 = 4000;
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
    /// `SFK1` set but `sample_hz` is not 25 or neither probe has a kind.
    Sfk1,
    /// An `sfk1_presets` byte is not a defined [`Sfk1Preset`] code.
    Sfk1Preset,
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
        sfk1_presets: [0; 2],
    };

    /// Serialize to the 22 schedule bytes.
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
        b[20..22].copy_from_slice(&self.sfk1_presets);
        b
    }

    /// Parse the 22 schedule bytes (e.g. `CheckInReply.schedule`). No
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
            sfk1_presets: [b[20], b[21]],
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

    /// `SFK1` flag set: windows carry SFK1 records.
    pub fn sfk1(&self) -> bool {
        self.flags & SCHEDULE_FLAG_SFK1 != 0
    }

    /// Kind of probe `port` (0 = probe 1, 1 = probe 2); `None` (the
    /// variant) for an undefined code, which [`validate`](Self::validate)
    /// rejects.
    pub fn probe(&self, port: usize) -> ProbeKind {
        let code = (self.flags >> SCHEDULE_PROBE_SHIFT[port]) & 0b11;
        ProbeKind::from_code(code).unwrap_or(ProbeKind::None)
    }

    /// `flags` with probe `port` set to `kind`.
    pub fn with_probe(mut self, port: usize, kind: ProbeKind) -> Self {
        let shift = SCHEDULE_PROBE_SHIFT[port];
        self.flags = (self.flags & !(0b11 << shift)) | (kind.code() << shift);
        self
    }

    /// SFK1 preset of probe `port`; the default for an undefined code, which
    /// [`validate`](Self::validate) rejects.
    pub fn preset(&self, port: usize) -> Sfk1Preset {
        Sfk1Preset::from_code(self.sfk1_presets[port]).unwrap_or_default()
    }

    /// With probe `port`'s SFK1 preset set to `preset`.
    pub fn with_preset(mut self, port: usize, preset: Sfk1Preset) -> Self {
        self.sfk1_presets[port] = preset.code();
        self
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
        if self.flags & !SCHEDULE_FLAGS_DEFINED != 0
            || SCHEDULE_PROBE_SHIFT
                .iter()
                .any(|&sh| ProbeKind::from_code((self.flags >> sh) & 0b11).is_none())
        {
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
        if self.sfk1()
            && (self.sample_hz != SCHEDULE_SFK1_SAMPLE_HZ
                || (0..2).all(|p| self.probe(p) == ProbeKind::None))
        {
            return Err(ScheduleError::Sfk1);
        }
        if self.sfk1_presets.iter().any(|&c| Sfk1Preset::from_code(c).is_none()) {
            return Err(ScheduleError::Sfk1Preset);
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

    /// SFK1 window ([`WINDOW_FLAG_SFK1`]): the data is SFK1 records.
    pub fn is_sfk1(&self) -> bool {
        self.window_flags & WINDOW_FLAG_SFK1 != 0
    }

    /// Window data length in bytes: `n_bins` for an SFK1 window, else
    /// `n_bins × channels × 2`.
    pub fn data_len(&self) -> usize {
        if self.is_sfk1() {
            self.n_bins as usize
        } else {
            window_data_len(self.n_bins, self.channels)
        }
    }

    /// `ceil(data_len / 100)`; equals `chunk_count` on a consistent header.
    pub fn expected_chunk_count(&self) -> usize {
        chunk_count(self.data_len())
    }

    /// Monotonic start time of bin `k`.
    pub fn bin_start_monotonic_us(&self, k: u32) -> u64 {
        self.start_monotonic_us + k as u64 * self.bin_ms() as u64 * 1000
    }

    /// Whether the header is self-consistent (13 channels, a bin width
    /// unless SFK1, matching chunk count).
    pub fn is_consistent(&self) -> bool {
        self.channels as usize == WINDOW_CHANNELS
            && (self.bin_ms_div10 != 0 || self.is_sfk1())
            && self.chunk_count as usize == self.expected_chunk_count()
    }

    /// Whether `data` has the expected length and matches `data_crc32`.
    pub fn verify_data(&self, data: &[u8]) -> bool {
        data.len() == self.data_len() && crc32(data) == self.data_crc32
    }
}

/// SFK1 window data that does not parse: an entry runs past the end, or a
/// probe number is not 1 or 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sfk1DataError {
    /// Byte offset of the bad entry.
    pub offset: usize,
}

/// The entries of SFK1 window data ([SFK1 window data](self#sfk1-window-data)):
/// `(probe, record)` with `probe` 1 or 2 and `record` empty if that probe
/// produced none. Yields one `Err` and stops at malformed data.
pub fn sfk1_records(data: &[u8]) -> Sfk1Records<'_> {
    Sfk1Records { data, offset: 0 }
}

/// Iterator returned by [`sfk1_records`].
#[derive(Debug, Clone)]
pub struct Sfk1Records<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for Sfk1Records<'a> {
    type Item = Result<(u8, &'a [u8]), Sfk1DataError>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.offset;
        let rest = self.data.get(at..).filter(|r| !r.is_empty())?;
        let entry = match rest {
            [probe @ (1 | 2), lo, hi, tail @ ..] => {
                let len = u16::from_le_bytes([*lo, *hi]) as usize;
                tail.get(..len).map(|rec| (*probe, rec))
            }
            _ => None,
        };
        match entry {
            Some((probe, rec)) => {
                self.offset = at + SFK1_ENTRY_PREFIX_LEN + rec.len();
                Some(Ok((probe, rec)))
            }
            None => {
                self.offset = self.data.len();
                Some(Err(Sfk1DataError { offset: at }))
            }
        }
    }
}

/// Write an SFK1 entry prefix (`probe`, `len`) into `out[..3]`.
pub fn write_sfk1_prefix(out: &mut [u8; SFK1_ENTRY_PREFIX_LEN], probe: u8, len: u16) {
    out[0] = probe;
    out[1..].copy_from_slice(&len.to_le_bytes());
}
