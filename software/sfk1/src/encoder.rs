//! The streaming integer encoder that runs on the board.
//!
//! # Arithmetic
//!
//! Every quantity is an integer with a stated scale, and every bound below
//! holds for *any* `i16` input and any channel count (channels never mix),
//! so nothing can overflow or panic:
//!
//! * ratio `p = raw / exc13` in Q24: `recip = (cnt << 40) / max(sum, cnt)`
//!   (one `u64` divide per frame), `p = raw * recip >> 16`, clamped to
//!   `|p| <= 2^30`. `exc13` is the mean of the present excitation samples in
//!   frames `k-6 ..= k+6` (shrinking at the window edges), so frame `k` is
//!   processed when frame `k+6` arrives.
//! * baseline bin `i`: `b = (16 E sum(p) / 2^24 + n) / (2 n)`, clamped to
//!   `[0, 2^B0_BITS)`; `E` (= `E_ref`) is the rounded mean excitation of the
//!   first non-empty bin. `|16 E sum(p)| <= 2^4 2^15 4096 2^30 = 2^61`.
//! * a missing post-onset frame is interpolated per channel between the
//!   previous present frame and the next one within 6 frames, in 1/16
//!   counts: `|16 (next - prev) (k - k_prev)| < 2^36`, and the result,
//!   between the two raw values, times `recip` stays below `2^60`.
//! * the line through `(bin centre, b / 8)` in closed form (i128, once per
//!   channel), then per post-onset sample in `p` units, Q16:
//!   `line(n) = A + B n`. For East30 `|A| < 2^57`, `|B n| < 2^62`; other
//!   profiles saturate instead of wrapping.
//! * the residual `r = p - line` (clamped to `i32`) is projected on the
//!   log-time spline basis as it arrives: `G_j += B_j(n) r` with
//!   `B_j <= 12 * 2^16 + 1`. For East30 `|G_j| <= max_j sum_n B_j(n) 2^31 <
//!   2^60.3`; the sum saturates for longer windows.
//! * at the end, `gx = (G >> 20) E >> 16` (`= 192 g`, clamped to `i32`), and
//!   `ci = round(sum_j MQ_kj gx_j / 2^MQ_SHIFT)` in i128, clamped to the
//!   escape range.
//!
//! For the East30 profiles nothing saturates, so the arithmetic is exactly
//! the Python mirror's (`fw_encode` in `encoder_spec.py`); golden vectors
//! check the two agree byte for byte.

use core::fmt;
use core::marker::PhantomData;

use crate::bits::{rice_len, BitWriter, Escape};
use crate::fixed;
use crate::profile::{assert_valid, Array, Profile};
use crate::{East30K12, East30K16, EXTENDED_VERSION};

/// The board encoder: 12 thermistors, [`East30K12`] (version `0x53`).
pub type Encoder12 = Encoder<12, East30K12>;
/// The board encoder with 16 knots: 12 thermistors, [`East30K16`] (`0x54`).
pub type Encoder16 = Encoder<12, East30K16>;

/// Half-width of the excitation boxcar (13 frames). An encoder detail: the
/// decoder never sees it.
const HALF: u16 = 6;
const RING: usize = 16;
const P_MAX: i64 = 1 << 30;

/// Why the encoder refused a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum EncodeError {
    /// The live mask has the wrong number of bytes.
    MaskLength {
        /// Bytes given.
        got: usize,
        /// Bytes needed, `ceil(N / 8)`.
        expected: usize,
        /// The encoder's channel count.
        n_channels: u16,
    },
    /// The live mask selects a channel above `N`.
    InvalidLiveMask {
        /// The first such channel (1-based).
        channel: u32,
        /// The encoder's channel count.
        n_channels: u16,
    },
    /// Baseline bins outside `2..=max`.
    InvalidBinCount {
        /// Bins asked for.
        n_bins: u8,
        /// The profile's limit, [`Profile::max_bins`].
        max: u8,
    },
    /// Every frame of the window has been pushed already.
    WindowFull,
    /// [`Encoder::finish`] before the last frame was pushed.
    Incomplete {
        /// Frames pushed so far.
        pushed: u16,
        /// Frames the window needs.
        expected: u16,
    },
    /// Every pre-onset frame was missing, so there is no baseline.
    NoBaseline,
    /// The output buffer is too small.
    BufferTooSmall {
        /// Record length in bytes.
        needed: usize,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MaskLength { got, expected, n_channels } => write!(
                f,
                "live mask is {got} bytes but N = {n_channels} needs {expected} (big-endian, bit c-1 = channel c)"
            ),
            Self::InvalidLiveMask { channel, n_channels } => {
                write!(f, "live mask has channel {channel} set but N = {n_channels}")
            }
            Self::InvalidBinCount { n_bins, max } => {
                write!(f, "{n_bins} baseline bins requested; the profile allows 2..={max}")
            }
            Self::WindowFull => f.write_str("window already complete; call finish, then reset for the next one"),
            Self::Incomplete { pushed, expected } => {
                write!(f, "window incomplete: {pushed} of {expected} frames pushed (use push_missing for gaps)")
            }
            Self::NoBaseline => f.write_str("no frame before onset was present, so there is no baseline"),
            Self::BufferTooSmall { needed } => write!(f, "output buffer too small: the record needs {needed} bytes"),
        }
    }
}

impl core::error::Error for EncodeError {}

/// What [`Encoder::finish`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct Finished {
    /// Record length in bytes (the record is `out[..len]`).
    pub len: usize,
    /// Frames pushed with [`Encoder::push_missing`].
    pub missing: u16,
    /// The reference excitation written to the header.
    pub e_ref: u16,
}

/// Streaming SFK1 encoder for one heat-pulse window of `N` thermistors
/// sharing one excitation, with the tables of profile `P`.
///
/// The window is `n_bins * BIN_FRAMES` frames before heater onset followed
/// by `POST_FRAMES` from onset, on the profile's fixed grid (East30: 25 Hz,
/// 60 s + 181 s = 6026 frames): start the encoder `n_bins` bins before
/// firing the heater, so that frame [`onset_frame`](Self::onset_frame) is
/// the first one at or after onset. Push exactly
/// [`frames_expected`](Self::frames_expected) frames, then
/// [`finish`](Self::finish).
///
/// Each push is constant-time integer work (the excitation boxcar, one
/// `u64` divide, the spline basis, 4 multiply-adds per live channel);
/// nothing allocates and nothing is buffered beyond 16 frames. The state is
/// about 220 B per channel for 12 knots (see the crate README for exact
/// `size_of` figures); a few hundred channels are fine on a host but too
/// big for the board's STM32G0B1.
///
/// # Missing frames
///
/// A dropped conversion is pushed with [`push_missing`](Self::push_missing)
/// so the grid stays aligned; the wire format does not change. A missing
/// frame is left out of the excitation boxcar and of its baseline bin (the
/// bin mean uses the frames present; an empty bin copies the nearest earlier
/// one, or the first non-empty one). After onset its counts are interpolated
/// linearly between the previous present frame and the next one within 6
/// frames, or held from the previous frame when there is none that close, so
/// that the projection sees every sample. The count is reported in
/// [`Finished::missing`].
///
/// # Several excitations
///
/// A sensor whose thermistors hang off several excitation sources (one per
/// ADC, say) runs one encoder per excitation group, each with its own
/// `E_ref`; the records are independent.
///
/// ```
/// use sfk1::{Encoder, Encoder12, East30K12, EncodeError};
///
/// // the board: thermistors 1..=5 of 12, 12 baseline bins
/// let enc = Encoder12::new(&0b1_1111u16.to_be_bytes(), 12)?;
/// assert_eq!(enc.frames_expected(), 6026);
///
/// // a 40-thermistor probe with the same geometry: 5-byte mask
/// let big = Encoder::<40, East30K12>::new(&[0xff; 5], 12)?;
/// assert_eq!(big.onset_frame(), 1500);
///
/// let err = Encoder12::new(&[0x10, 0x00], 12).unwrap_err();
/// assert_eq!(err.to_string(), "live mask has channel 13 set but N = 12");
/// # Ok::<(), EncodeError>(())
/// ```
pub struct Encoder<const N: usize, P: Profile> {
    live: [bool; N],
    n_bins: u8,
    pushed: u16,
    processed: u16,
    missing: u16,
    /// presence of ring slot `frame & 15`
    present: u16,
    exc: [i16; RING],
    therm: [[i16; N]; 8],
    prev_raw: [i16; N],
    prev_frame: Option<u16>,
    bin_n: u16,
    bin_exc: i32,
    bin_sum: [i64; N],
    e_ref: Option<u16>,
    empty_bins: u64,
    baseline_ok: bool,
    bins: [P::Bins; N],
    line_a: [i64; N],
    line_b: [i64; N],
    last_p: [i32; N],
    g: [P::Acc; N],
    _profile: PhantomData<P>,
}

impl<const N: usize, P: Profile> fmt::Debug for Encoder<N, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encoder")
            .field("profile", &P::NAME)
            .field("n_channels", &N)
            .field("n_bins", &self.n_bins)
            .field("pushed", &self.pushed)
            .field("missing", &self.missing)
            .finish_non_exhaustive()
    }
}

#[inline]
#[allow(clippy::cast_possible_truncation)]
fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[inline]
#[allow(clippy::cast_possible_truncation)]
fn clamp_i64(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// `round(x / d)`, halves up, for `d > 0` (Python's `(2x + d) // 2d`);
/// 0 for `d = 0`, which never happens. Out of line: on Cortex-M0 an `i128`
/// division is a few hundred bytes of code, so it is emitted once.
#[inline(never)]
fn round_div(x: i128, d: i128) -> i128 {
    (2 * x + d).checked_div_euclid(2 * d).unwrap_or(0)
}

/// [`round_div`] in `i64`, inlined (`|x| < 2^61`).
#[inline]
fn round_div64(x: i64, d: i64) -> i64 {
    (2 * x + d).checked_div_euclid(2 * d).unwrap_or(0)
}

/// Bits of the longest Rice code with parameter `k` (escape or not).
const fn max_code(k: u32, esc_q: u32, esc_bits: u32) -> usize {
    let raw = if k > esc_bits { k } else { esc_bits };
    (esc_q + raw) as usize
}

impl<const N: usize, P: Profile> Encoder<N, P> {
    /// Compile-time check of `P` and `N`: building an encoder for a broken
    /// profile fails the build with the rule it broke.
    const VALID: () = {
        assert_valid::<P>();
        assert!(N >= 1 && N <= 65_535, "Encoder: N must be in 1..=65535");
    };

    const MASK_BYTES: usize = N.div_ceil(8);

    /// Whether this encoder writes the compact East30 header.
    const COMPACT: bool = P::COMPACT_VERSION.is_some() && N == 12;

    const LOG2_T0: i64 = fixed::log2_q30(P::T0_FRAMES);

    /// Header bytes this encoder writes: 6 (compact, for the East30
    /// profiles with `N = 12`) or `10 + ceil(N / 8)` (extended).
    pub const HEADER_LEN: usize = if Self::COMPACT { 6 } else { 10 + Self::MASK_BYTES };

    /// A buffer this long holds any record this encoder can produce (all
    /// channels live, the most bins, every Rice code at its longest); 2090
    /// bytes for [`Encoder16`]. Typical records are about 20 B per channel.
    pub const MAX_RECORD_LEN: usize = {
        let mut bits = P::B0_BITS as usize + 4 + P::OFFSET_BITS as usize;
        bits += (<P::Bins as Array<i32>>::LEN - 1) * max_code(15, P::ESCAPE_Q, P::ESCAPE_BITS);
        let o_max = P::OFFSET_MIN + (1 << P::OFFSET_BITS) - 1;
        let mut j = 0;
        while j < P::K_TABLE.len() {
            let k = P::K_TABLE[j] as i32 + o_max;
            #[allow(clippy::cast_sign_loss)]
            let k = if k < 0 { 0 } else { k as u32 };
            bits += max_code(k, P::ESCAPE_Q, P::ESCAPE_BITS);
            j += 1;
        }
        Self::HEADER_LEN + (N * bits).div_ceil(8)
    };

    /// An encoder for one window.
    ///
    /// `live_mask` selects the channels to encode: `ceil(N / 8)` bytes,
    /// big-endian, bit `c - 1` = channel `c` (the same bytes as the record
    /// header; for the 12-channel board, `&mask_u16.to_be_bytes()`).
    /// `n_bins` is the baseline length in bins, usually
    /// [`Profile::DEFAULT_BINS`].
    ///
    /// # Errors
    ///
    /// [`EncodeError::MaskLength`], [`EncodeError::InvalidLiveMask`] or
    /// [`EncodeError::InvalidBinCount`].
    pub const fn new(live_mask: &[u8], n_bins: u8) -> Result<Self, EncodeError> {
        let () = Self::VALID;
        #[allow(clippy::cast_possible_truncation)] // N <= 65535
        let n_channels = N as u16;
        if live_mask.len() != Self::MASK_BYTES {
            return Err(EncodeError::MaskLength { got: live_mask.len(), expected: Self::MASK_BYTES, n_channels });
        }
        let mut live = [false; N];
        let mut p = 0;
        while p < 8 * Self::MASK_BYTES {
            if live_mask[Self::MASK_BYTES - 1 - p / 8] >> (p % 8) & 1 == 1 {
                if p >= N {
                    #[allow(clippy::cast_possible_truncation)]
                    return Err(EncodeError::InvalidLiveMask { channel: p as u32 + 1, n_channels });
                }
                live[p] = true;
            }
            p += 1;
        }
        Self::with_live(live, n_bins)
    }

    const fn with_live(live: [bool; N], n_bins: u8) -> Result<Self, EncodeError> {
        let max = <P::Bins as Array<i32>>::LEN;
        if n_bins < 2 || n_bins as usize > max {
            #[allow(clippy::cast_possible_truncation)] // max <= 64
            return Err(EncodeError::InvalidBinCount { n_bins, max: max as u8 });
        }
        Ok(Self {
            live,
            n_bins,
            pushed: 0,
            processed: 0,
            missing: 0,
            present: 0,
            exc: [0; RING],
            therm: [[0; N]; 8],
            prev_raw: [0; N],
            prev_frame: None,
            bin_n: 0,
            bin_exc: 0,
            bin_sum: [0; N],
            e_ref: None,
            empty_bins: 0,
            baseline_ok: false,
            bins: [<P::Bins as Array<i32>>::ZERO; N],
            line_a: [0; N],
            line_b: [0; N],
            last_p: [0; N],
            g: [<P::Acc as Array<i64>>::ZERO; N],
            _profile: PhantomData,
        })
    }

    /// Start a new window with the same channels and bin count.
    pub fn reset(&mut self) {
        if let Ok(fresh) = Self::with_live(self.live, self.n_bins) {
            *self = fresh;
        }
    }

    /// Whether channel `c` (1-based) is encoded.
    #[must_use]
    pub const fn is_live(&self, c: usize) -> bool {
        c >= 1 && c <= N && self.live[c - 1]
    }

    /// Baseline bins before onset.
    #[must_use]
    pub const fn n_bins(&self) -> u8 {
        self.n_bins
    }

    /// Index of the heater-onset frame (t = 0): `n_bins * BIN_FRAMES`.
    #[must_use]
    pub const fn onset_frame(&self) -> u16 {
        // MAX_BINS * BIN_FRAMES + POST_FRAMES <= 65535 is a profile invariant
        self.n_bins as u16 * P::BIN_FRAMES
    }

    /// Frames the window takes: `n_bins * BIN_FRAMES + POST_FRAMES`.
    #[must_use]
    pub const fn frames_expected(&self) -> u16 {
        self.onset_frame() + P::POST_FRAMES
    }

    /// Frames pushed so far, present or missing.
    #[must_use]
    pub const fn frames_pushed(&self) -> u16 {
        self.pushed
    }

    /// Frames pushed with [`push_missing`](Self::push_missing) so far.
    #[must_use]
    pub const fn missing_frames(&self) -> u16 {
        self.missing
    }

    /// Whether every frame has been pushed, so [`finish`](Self::finish) can run.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.processed == self.frames_expected()
    }

    /// The reference excitation, once the first non-empty baseline bin is done.
    #[must_use]
    pub const fn e_ref(&self) -> Option<u16> {
        self.e_ref
    }

    /// Push the next frame: the excitation and every thermistor's raw ADC
    /// counts (channels not in the live mask are ignored).
    ///
    /// # Errors
    ///
    /// [`EncodeError::WindowFull`] once the window is complete.
    pub fn push(&mut self, exc: i16, therm: &[i16; N]) -> Result<(), EncodeError> {
        self.store(Some((exc, therm)))
    }

    /// Push a placeholder for a frame that was not sampled (see
    /// [Missing frames](Self#missing-frames)).
    ///
    /// # Errors
    ///
    /// [`EncodeError::WindowFull`] once the window is complete.
    pub fn push_missing(&mut self) -> Result<(), EncodeError> {
        self.store(None)
    }

    fn store(&mut self, frame: Option<(i16, &[i16; N])>) -> Result<(), EncodeError> {
        let total = self.frames_expected();
        if self.pushed >= total {
            return Err(EncodeError::WindowFull);
        }
        let f = self.pushed;
        let slot = usize::from(f) % RING;
        if let Some((exc, therm)) = frame {
            self.exc[slot] = exc;
            self.therm[usize::from(f) % 8] = *therm;
            self.present |= 1 << slot;
        } else {
            self.present &= !(1 << slot);
            self.missing += 1;
        }
        self.pushed += 1;
        let ready = if self.pushed == total { total } else { self.pushed.saturating_sub(HALF) };
        while self.processed < ready {
            self.process(self.processed);
            self.processed += 1;
        }
        Ok(())
    }

    fn is_present(&self, k: u16) -> bool {
        self.present >> (usize::from(k) % RING) & 1 == 1
    }

    /// `(cnt << 40) / max(sum, cnt)` over the present excitation samples in
    /// frames `k-6 ..= k+6` (inside the window); `<= 2^40`.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    fn recip(&self, k: u16) -> i64 {
        let hi = (k + HALF).min(self.frames_expected() - 1);
        let (mut sum, mut cnt) = (0i32, 0i32);
        for j in k.saturating_sub(HALF)..=hi {
            if self.is_present(j) {
                sum += i32::from(self.exc[usize::from(j) % RING]);
                cnt += 1;
            }
        }
        let cnt = cnt.max(1);
        (((cnt as u64) << 40) / (sum.max(cnt) as u64)) as i64
    }

    #[allow(clippy::cast_possible_truncation)]
    fn ratio(raw: i64, recip: i64) -> i32 {
        // |raw| <= 2^15, recip <= 2^40
        ((raw * recip) >> 16).clamp(-P_MAX, P_MAX) as i32
    }

    fn process(&mut self, k: u16) {
        let onset = self.onset_frame();
        let recip = self.recip(k);
        if self.is_present(k) {
            let raw = self.therm[usize::from(k) % 8];
            for (c, &r) in raw.iter().enumerate() {
                if !self.live[c] {
                    continue;
                }
                let p = Self::ratio(i64::from(r), recip);
                if k < onset {
                    self.bin_sum[c] += i64::from(p);
                }
                self.last_p[c] = p;
            }
            if k < onset {
                self.bin_n += 1;
                self.bin_exc += i32::from(self.exc[usize::from(k) % RING]);
            }
            self.prev_raw = raw;
            self.prev_frame = Some(k);
        } else if k >= onset {
            self.fill_missing(k, recip);
        }
        if k < onset {
            if (k + 1).is_multiple_of(P::BIN_FRAMES) {
                self.finish_bin(usize::from(k / P::BIN_FRAMES));
                if k + 1 == onset {
                    self.start_rise();
                }
            }
        } else if k > onset {
            self.accumulate(k - onset);
        }
    }

    /// Interpolate a missing post-onset frame from the previous present
    /// frame and the next one within 6 frames (1/16-count raw values);
    /// otherwise keep `last_p` (hold).
    fn fill_missing(&mut self, k: u16, recip: i64) {
        let Some(kp) = self.prev_frame else { return };
        let hi = (k + HALF).min(self.frames_expected() - 1);
        let Some(kn) = (k + 1..=hi).find(|&j| self.is_present(j)) else { return };
        let next = usize::from(kn) % 8;
        let (num, den) = (i64::from(k - kp), i64::from(kn - kp));
        for c in 0..N {
            if !self.live[c] {
                continue;
            }
            // |16 (next - a) num| < 2^4 2^16 2^16 = 2^36: no overflow in i64
            let a = i64::from(self.prev_raw[c]);
            let step = round_div64(16 * (i64::from(self.therm[next][c]) - a) * num, den);
            // |r16| <= 2^20 and recip <= 2^40
            let r16 = 16 * a + step;
            #[allow(clippy::cast_possible_truncation)]
            let p = ((r16 * recip) >> 20).clamp(-P_MAX, P_MAX) as i32;
            self.last_p[c] = p;
        }
    }

    fn finish_bin(&mut self, i: usize) {
        let n = i64::from(self.bin_n);
        if n == 0 {
            self.empty_bins |= 1 << i;
        } else {
            let exc = i128::from(self.bin_exc);
            // E_ref: rounded mean excitation of the first non-empty bin
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let e = *self.e_ref.get_or_insert_with(|| round_div(exc, i128::from(n)).clamp(0, 32_767) as u16);
            let b_max = (1i32 << P::B0_BITS) - 1;
            for c in 0..N {
                if !self.live[c] {
                    continue;
                }
                let t = (16 * i64::from(e) * self.bin_sum[c] + (n << 24)) >> 25;
                #[allow(clippy::cast_possible_truncation)] // n <= 4096
                let b = clamp_i32(t).checked_div_euclid(n as i32).unwrap_or(0).clamp(0, b_max);
                if let Some(slot) = self.bins[c].as_mut().get_mut(i) {
                    *slot = b;
                }
            }
        }
        self.bin_n = 0;
        self.bin_exc = 0;
        self.bin_sum = [0; N];
    }

    /// At onset: fill empty bins and fit each channel's baseline line.
    fn start_rise(&mut self) {
        let nb = usize::from(self.n_bins);
        let empty = self.empty_bins;
        let Some(first_ok) = (0..nb).find(|&i| empty >> i & 1 == 0) else {
            self.baseline_ok = false;
            return;
        };
        self.baseline_ok = true;
        let e = i128::from(self.e_ref.unwrap_or(0).max(1));
        let nbi = i128::from(self.n_bins);
        let bf = i128::from(P::BIN_FRAMES);
        let w2 = nbi * (nbi * nbi - 1) / 3; // sum of w_i^2, w_i = 2i - (nb - 1)
        for c in 0..N {
            if !self.live[c] {
                continue;
            }
            // nb <= LEN (checked in new)
            let Some(b) = self.bins[c].as_mut().get_mut(..nb) else { continue };
            let mut fill = b.get(first_ok).copied().unwrap_or(0);
            for (i, v) in b.iter_mut().enumerate() {
                if empty >> i & 1 == 0 {
                    fill = *v;
                } else {
                    *v = fill;
                }
            }
            let (mut p_sum, mut s1) = (0i128, 0i128);
            for (i, &v) in b.iter().enumerate() {
                #[allow(clippy::cast_possible_wrap)]
                let w = 2 * i as i128 - (nbi - 1);
                p_sum += i128::from(v);
                s1 += w * i128::from(v);
            }
            // least squares through (bin centre, b / 8), centres at frames
            // BF i + (BF - 1) / 2 - BF nb; value at onset and slope per
            // frame, Q16 / Q32 counts, then in p units (Q24 ratio), Q16
            let a_q16 = round_div((p_sum * bf * w2 + s1 * (bf * nbi + 1) * nbi) << 16, 8 * bf * nbi * w2);
            let bn_q32 = round_div(s1 << 32, 4 * bf * w2);
            self.line_a[c] = clamp_i64(round_div(a_q16 << 24, e));
            self.line_b[c] = clamp_i64(round_div(bn_q32 << 8, e));
        }
    }

    fn accumulate(&mut self, n: u16) {
        let intervals = usize::from(P::KNOTS) + 1;
        let (i0, b) = fixed::basis(n, P::T0_FRAMES, Self::LOG2_T0, P::KF_Q28, intervals);
        for c in 0..N {
            if !self.live[c] {
                continue;
            }
            let line =
                self.line_a[c].saturating_add(self.line_b[c].saturating_mul(i64::from(n))).saturating_add(1 << 15)
                    >> 16;
            let r = i64::from(clamp_i32(i64::from(self.last_p[c]).saturating_sub(line)));
            if let Some(g) = self.g[c].as_mut().get_mut(i0..i0 + 4) {
                for (acc, &v) in g.iter_mut().zip(b.iter()) {
                    *acc = acc.saturating_add(i64::from(v) * r);
                }
            }
        }
    }

    /// Quantized coefficient `k` of channel `c`.
    fn coef(&self, c: usize, k: usize, e: u16) -> i32 {
        let nc = P::n_coefs();
        let row = P::MQ.get(k * nc..(k + 1) * nc).unwrap_or(&[]);
        let acc: i128 = row
            .iter()
            .zip(self.g[c].as_ref())
            .map(|(&m, &g)| i128::from(m) * i128::from(clamp_i32(((g >> 20) * i64::from(e)) >> 16)))
            .sum();
        let half = 1i128 << (P::ESCAPE_BITS - 1);
        #[allow(clippy::cast_possible_truncation)] // clamped to the escape range (<= 24 bits)
        let q = ((acc + (1 << (P::MQ_SHIFT - 1))) >> P::MQ_SHIFT).clamp(-half, half - 1) as i32;
        q
    }

    /// Write the record into `out` and return its length.
    ///
    /// Takes `&self`: on [`EncodeError::BufferTooSmall`] call again with a
    /// bigger buffer. [`MAX_RECORD_LEN`](Self::MAX_RECORD_LEN) bytes always
    /// suffice.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Incomplete`], [`EncodeError::NoBaseline`] or
    /// [`EncodeError::BufferTooSmall`].
    pub fn finish(&self, out: &mut [u8]) -> Result<Finished, EncodeError> {
        if !self.is_complete() {
            return Err(EncodeError::Incomplete { pushed: self.pushed, expected: self.frames_expected() });
        }
        if !self.baseline_ok {
            return Err(EncodeError::NoBaseline);
        }
        let e = self.e_ref.unwrap_or(0);
        let nb = usize::from(self.n_bins);
        let esc = Escape::of::<P>();
        let cap = out.len();
        let mut w = BitWriter::new(out);
        let mask_bits = match P::COMPACT_VERSION {
            Some(v) if Self::COMPACT => {
                w.put(u32::from(v), 8);
                16
            }
            _ => {
                w.put(u32::from(EXTENDED_VERSION), 8);
                w.put(P::ID, 32);
                #[allow(clippy::cast_possible_truncation)] // N <= 65535
                w.put(N as u32, 16);
                8 * Self::MASK_BYTES
            }
        };
        for p in (0..mask_bits).rev() {
            w.put(u32::from(p < N && self.live[p]), 1);
        }
        w.put(u32::from(e), 16);
        w.put(u32::from(self.n_bins), 8);
        #[allow(clippy::cast_sign_loss)]
        let k_of = |j: usize, o: i32| (P::K_TABLE.get(j).map_or(0, |&k| i32::from(k)) + o).max(0) as u32;
        for c in 0..N {
            if !self.live[c] {
                continue;
            }
            // nb <= LEN (checked in new) and n_coefs <= MAX_COEFS (checked
            // with the profile), so neither `else` is taken
            let Some(b) = self.bins[c].as_ref().get(..nb) else { continue };
            let deltas = || b.windows(2).map(|pair| pair[1] - pair[0]);
            let kb = (0..16u32).min_by_key(|&k| deltas().map(|v| rice_len(v, k, esc)).sum::<u32>()).unwrap_or(0);
            let mut ci = [0i32; crate::MAX_COEFS];
            let Some(ci) = ci.get_mut(..P::n_coefs()) else { continue };
            for (k, v) in ci.iter_mut().enumerate() {
                *v = self.coef(c, k, e);
            }
            let o = (0..1i32 << P::OFFSET_BITS)
                .map(|f| f + P::OFFSET_MIN)
                .min_by_key(|&o| ci.iter().enumerate().map(|(j, &v)| rice_len(v, k_of(j, o), esc)).sum::<u32>())
                .unwrap_or(0);
            #[allow(clippy::cast_sign_loss)] // b >= 0
            w.put(b.first().map_or(0, |&b0| b0 as u32), P::B0_BITS);
            w.put(kb, 4);
            for v in deltas() {
                w.rice(v, kb, esc);
            }
            #[allow(clippy::cast_sign_loss)]
            w.put((o - P::OFFSET_MIN) as u32, P::OFFSET_BITS);
            for (j, &v) in ci.iter().enumerate() {
                w.rice(v, k_of(j, o), esc);
            }
        }
        let len = w.pad();
        if len > cap {
            return Err(EncodeError::BufferTooSmall { needed: len });
        }
        Ok(Finished { len, missing: self.missing, e_ref: e })
    }
}
