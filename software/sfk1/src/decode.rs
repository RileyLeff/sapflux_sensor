//! Record parsing and the f64 reconstruction (feature `decode`).
//!
//! The core API needs no allocator and learns the channel count from the
//! record: [`Decoder::new`] validates a record once,
//! [`Decoder::channels`] yields each channel's integers as a
//! [`ChannelCoefs`], and [`ChannelCoefs::reconstruct`] (or
//! [`ChannelCoefs::samples`]) rebuilds the curve. With `alloc`, [`decode()`]
//! picks the built-in profile from the header and returns everything in
//! [`Vec`](alloc::vec::Vec)s.
//!
//! Reconstruction (in counts at `E_ref`, time `t` in s from onset, `BF` the
//! profile's bin frames and `fs` its rate):
//!
//! * `line(t)`: least squares through the bin centres
//!   `((BF i + (BF - 1) / 2 - BF n_bins) / fs, b_i / 8)`;
//! * before onset: `line(t)` plus the bin residuals `b_i / 8 - line`,
//!   interpolated linearly between bin centres and held beyond the ends;
//! * from onset: `line(t) + sum_j B_j(t) w_j` with `w = D (STEP ci)`, `B_j`
//!   the cubic B-splines in `ln(t + t0)` (uniform interior knots over
//!   `[ln t0, ln(t_end + t0) + 1e-9]`, zero at `t = 0`).

use core::fmt;
use core::marker::PhantomData;

use crate::bits::{BitReader, Escape, Truncated};
use crate::header::{Header, Layout};
use crate::profile::{assert_valid, Profile};
use crate::tables::BASIS_C12;
use crate::MAX_COEFS;

/// Most baseline bins a record may declare and still be decoded.
pub const MAX_DECODE_BINS: usize = 64;

/// Why a record was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum DecodeError {
    /// The record ends inside a field.
    Truncated,
    /// The version byte is not `0x53`, `0x54` or `0x5F`.
    UnsupportedVersion(u8),
    /// The extended header declares 0 channels.
    InvalidChannelCount(u16),
    /// The live mask selects a channel above the record's channel count.
    InvalidLiveMask {
        /// The first such channel (1-based).
        channel: u32,
        /// The record's channel count.
        n_channels: u16,
    },
    /// Fewer than 2 or more than [`MAX_DECODE_BINS`] baseline bins.
    InvalidBinCount(u8),
    /// The record was made with another profile than the decoder's.
    WrongProfile {
        /// The record's header layout (version byte or profile id).
        found: Layout,
        /// The decoder's profile name.
        expected: &'static str,
    },
    /// Bytes follow the last channel.
    TrailingBytes,
    /// The padding bits after the last channel are not zero.
    NonZeroPadding,
    /// An output buffer has the wrong length.
    BufferLength {
        /// Required length, the record's frame count.
        expected: usize,
        /// Length given.
        got: usize,
    },
}

impl From<Truncated> for DecodeError {
    fn from(_: Truncated) -> Self {
        Self::Truncated
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Truncated => f.write_str("record truncated"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported record version {v:#04x} (known: 0x53, 0x54, extended 0x5f)")
            }
            Self::InvalidChannelCount(n) => write!(f, "record declares {n} channels"),
            Self::InvalidLiveMask { channel, n_channels } => {
                write!(f, "live mask has channel {channel} set but N = {n_channels}")
            }
            Self::InvalidBinCount(n) => write!(f, "{n} baseline bins (need 2..={MAX_DECODE_BINS})"),
            Self::WrongProfile { found: Layout::Compact { version }, expected } => {
                write!(f, "record version {version:#04x} is not the {expected} profile's")
            }
            Self::WrongProfile { found: Layout::Extended { profile_id }, expected } => {
                write!(f, "record profile id {profile_id:#010x} is not the {expected} profile's")
            }
            Self::TrailingBytes => f.write_str("trailing bytes after the last channel"),
            Self::NonZeroPadding => f.write_str("non-zero padding bits"),
            Self::BufferLength { expected, got } => {
                write!(f, "buffer holds {got} samples but the record has {expected} frames")
            }
        }
    }
}

impl core::error::Error for DecodeError {}

/// A validated SFK1 record of profile `P`, any channel count.
///
/// ```
/// use sfk1::{Decoder, East30K12, Encoder};
///
/// // a 20-channel window (extended header), channels 1 and 20 live
/// let mut enc = Encoder::<20, East30K12>::new(&[0x08, 0x00, 0x01], 2)?;
/// for _ in 0..enc.frames_expected() {
///     enc.push(25_000, &[12_500; 20])?;
/// }
/// let mut out = [0u8; 256];
/// let len = enc.finish(&mut out)?.len;
/// let rec = &out[..len];
///
/// let dec = Decoder::<East30K12>::new(rec)?;
/// assert_eq!(dec.header().n_channels(), 20);
/// let chans: Vec<u16> = dec.channels().map(|c| c.channel()).collect();
/// assert_eq!(chans, [1, 20]);
/// for ch in dec.channels() {
///     assert!(ch.samples().all(|v| (v - 12_500.0).abs() < 1e-6));
/// }
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
pub struct Decoder<'a, P: Profile> {
    header: Header<'a>,
    data: &'a [u8],
    _profile: PhantomData<fn() -> P>,
}

impl<P: Profile> Clone for Decoder<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Profile> Copy for Decoder<'_, P> {}

impl<P: Profile> fmt::Debug for Decoder<'_, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoder").field("profile", &P::NAME).field("header", &self.header).finish()
    }
}

/// The profile's layout matches a header.
fn profile_matches<P: Profile>(layout: Layout) -> bool {
    match layout {
        Layout::Compact { version } => P::COMPACT_VERSION == Some(version),
        Layout::Extended { profile_id } => profile_id == P::ID,
    }
}

impl<'a, P: Profile> Decoder<'a, P> {
    const VALID: () = assert_valid::<P>();

    /// Parse and fully validate `record` (header, profile, every channel,
    /// padding and length), so that [`channels`](Self::channels) cannot
    /// fail.
    ///
    /// # Errors
    ///
    /// Any [`DecodeError`] except [`DecodeError::BufferLength`].
    pub fn new(record: &'a [u8]) -> Result<Self, DecodeError> {
        let () = Self::VALID;
        let header = Header::parse(record)?;
        if !profile_matches::<P>(header.layout()) {
            return Err(DecodeError::WrongProfile { found: header.layout(), expected: P::NAME });
        }
        let mut r = Self::reader(record, &header);
        for _ in 0..header.n_live() {
            read_channel::<P>(&mut r, header.n_bins(), 0)?;
        }
        let bits = r.bit_pos();
        let len = bits.div_ceil(8);
        if record.len() > header.byte_len() + len {
            return Err(DecodeError::TrailingBytes);
        }
        if r.get(u32::try_from(8 * len - bits).unwrap_or(0))? != 0 {
            return Err(DecodeError::NonZeroPadding);
        }
        Ok(Self { header, data: record, _profile: PhantomData })
    }

    fn reader(record: &'a [u8], header: &Header<'_>) -> BitReader<'a> {
        BitReader::new(record.get(header.byte_len()..).unwrap_or(&[]))
    }

    /// The record header.
    #[must_use]
    pub const fn header(&self) -> &Header<'a> {
        &self.header
    }

    /// Frame index of heater onset: `n_bins * BIN_FRAMES`.
    #[must_use]
    pub fn onset_frame(&self) -> usize {
        usize::from(self.header.n_bins()) * usize::from(P::BIN_FRAMES)
    }

    /// Frames per reconstructed channel: `onset_frame() + POST_FRAMES`.
    #[must_use]
    pub fn n_frames(&self) -> usize {
        self.onset_frame() + usize::from(P::POST_FRAMES)
    }

    /// Time of frame `i` in seconds from heater onset (negative before it).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn time_s(&self, i: usize) -> f64 {
        P::frame_time_s(i as f64 - self.onset_frame() as f64)
    }

    /// The record's channels, in channel order.
    #[must_use]
    pub fn channels(&self) -> Channels<'a, P> {
        Channels { reader: Self::reader(self.data, &self.header), header: self.header, next: 1, _profile: PhantomData }
    }
}

/// Iterator over a record's channels, from [`Decoder::channels`].
pub struct Channels<'a, P: Profile> {
    header: Header<'a>,
    reader: BitReader<'a>,
    next: u32,
    _profile: PhantomData<fn() -> P>,
}

impl<P: Profile> Iterator for Channels<'_, P> {
    type Item = ChannelCoefs<P>;

    fn next(&mut self) -> Option<ChannelCoefs<P>> {
        let n = u32::from(self.header.n_channels());
        #[allow(clippy::cast_possible_truncation)] // c <= n <= 65535
        let c = (self.next..=n).find(|&c| self.header.is_live(c as u16))? as u16;
        self.next = u32::from(c) + 1;
        // validated in Decoder::new
        read_channel::<P>(&mut self.reader, self.header.n_bins(), c).ok()
    }
}

fn read_channel<P: Profile>(r: &mut BitReader<'_>, n_bins: u8, channel: u16) -> Result<ChannelCoefs<P>, DecodeError> {
    let esc = Escape::of::<P>();
    let nb = usize::from(n_bins);
    let mut bins = [0i32; MAX_DECODE_BINS];
    #[allow(clippy::cast_possible_wrap)] // B0_BITS <= 23
    let b0 = r.get(P::B0_BITS)? as i32;
    bins[0] = b0;
    let kb = r.get(4)?;
    for i in 1..nb {
        // |b| < 64 * 2^23: no overflow
        bins[i] = bins[i - 1] + r.rice(kb, esc)?;
    }
    #[allow(clippy::cast_possible_wrap)] // OFFSET_BITS <= 4
    let o = r.get(P::OFFSET_BITS)? as i32 + P::OFFSET_MIN;
    let mut coefs = [0i32; MAX_COEFS];
    for (ci, &k) in coefs.iter_mut().zip(P::K_TABLE) {
        #[allow(clippy::cast_sign_loss)]
        let k = (i32::from(k) + o).max(0) as u32;
        *ci = r.rice(k, esc)?;
    }
    Ok(ChannelCoefs { channel, n_bins, bins, coefs, _profile: PhantomData })
}

/// The baseline line `intercept + slope * t` (counts, `t` in s from onset).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Line {
    /// Value at onset, counts.
    pub intercept: f64,
    /// Drift, counts per second.
    pub slope: f64,
}

impl Line {
    /// The line at `t` seconds from onset.
    ///
    /// ```
    /// let l = sfk1::Line { intercept: 10.0, slope: 0.5 };
    /// assert_eq!(l.at(4.0), 12.0);
    /// ```
    #[must_use]
    pub fn at(&self, t: f64) -> f64 {
        self.intercept + self.slope * t
    }
}

/// One channel of a record: the transmitted integers and the decoder.
pub struct ChannelCoefs<P: Profile> {
    channel: u16,
    n_bins: u8,
    bins: [i32; MAX_DECODE_BINS],
    coefs: [i32; MAX_COEFS],
    _profile: PhantomData<fn() -> P>,
}

impl<P: Profile> Clone for ChannelCoefs<P> {
    fn clone(&self) -> Self {
        Self { _profile: PhantomData, ..*self }
    }
}

impl<P: Profile> PartialEq for ChannelCoefs<P> {
    fn eq(&self, o: &Self) -> bool {
        self.channel == o.channel && self.bins() == o.bins() && self.coefs() == o.coefs()
    }
}

impl<P: Profile> fmt::Debug for ChannelCoefs<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelCoefs")
            .field("channel", &self.channel)
            .field("bins", &self.bins())
            .field("coefs", &self.coefs())
            .finish_non_exhaustive()
    }
}

/// Bin centre `i` in seconds from onset.
#[allow(clippy::cast_precision_loss)]
fn bin_centre<P: Profile>(i: usize, n_bins: usize) -> f64 {
    let bf = f64::from(P::BIN_FRAMES);
    P::frame_time_s(bf * i as f64 + (bf - 1.0) / 2.0 - bf * n_bins as f64)
}

impl<P: Profile> ChannelCoefs<P> {
    /// Channel number, 1-based.
    #[must_use]
    pub const fn channel(&self) -> u16 {
        self.channel
    }

    /// Baseline bin means, 1/8 counts at `E_ref` (`n_bins` values).
    #[must_use]
    pub fn bins(&self) -> &[i32] {
        &self.bins[..usize::from(self.n_bins)]
    }

    /// Quantized rise coefficients (`KNOTS + 4` values, `STEP` counts each).
    #[must_use]
    pub fn coefs(&self) -> &[i32] {
        &self.coefs[..P::n_coefs()]
    }

    /// Frames in the reconstruction: `n_bins * BIN_FRAMES + POST_FRAMES`.
    #[must_use]
    pub fn n_frames(&self) -> usize {
        usize::from(self.n_bins) * usize::from(P::BIN_FRAMES) + usize::from(P::POST_FRAMES)
    }

    /// The baseline line (least squares through the bin centres).
    #[must_use]
    pub fn line(&self) -> Line {
        let nb = usize::from(self.n_bins);
        #[allow(clippy::cast_precision_loss)]
        let n = nb as f64;
        let (mut st, mut sy) = (0.0, 0.0);
        for (i, &b) in self.bins().iter().enumerate() {
            st += bin_centre::<P>(i, nb);
            sy += f64::from(b) / 8.0;
        }
        let (tbar, ybar) = (st / n, sy / n);
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for (i, &b) in self.bins().iter().enumerate() {
            let dt = bin_centre::<P>(i, nb) - tbar;
            sxy += dt * (f64::from(b) / 8.0 - ybar);
            sxx += dt * dt;
        }
        let slope = sxy / sxx;
        Line { intercept: ybar - slope * tbar, slope }
    }

    /// The reconstruction, one value per frame from the window start
    /// (counts at `E_ref`).
    #[must_use]
    pub fn samples(&self) -> Samples<'_, P> {
        let nc = P::n_coefs();
        let mut w = [0.0f64; MAX_COEFS];
        for (j, wj) in w.iter_mut().enumerate().take(nc) {
            *wj = (0..nc).map(|k| P::D[j * nc + k] * f64::from(self.coefs[k]) * P::STEP).sum();
        }
        let t0 = P::frame_time_s(f64::from(P::T0_FRAMES));
        let t_end = P::frame_time_s(f64::from(P::POST_FRAMES - 1));
        let u0 = libm::log(t0);
        let u1 = libm::log(t_end + t0) + 1e-9;
        Samples { ch: self, line: self.line(), w, t0, u0, u1, frame: 0 }
    }

    /// Write the reconstruction into `out`, which must be
    /// [`n_frames`](Self::n_frames) long; `out[i]` is at
    /// [`Decoder::time_s`]`(i)`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::BufferLength`] if `out` has the wrong length.
    pub fn reconstruct(&self, out: &mut [f64]) -> Result<(), DecodeError> {
        let expected = self.n_frames();
        if out.len() != expected {
            return Err(DecodeError::BufferLength { expected, got: out.len() });
        }
        for (o, v) in out.iter_mut().zip(self.samples()) {
            *o = v;
        }
        Ok(())
    }
}

/// Iterator over a channel's reconstruction, from [`ChannelCoefs::samples`].
pub struct Samples<'c, P: Profile> {
    ch: &'c ChannelCoefs<P>,
    line: Line,
    w: [f64; MAX_COEFS],
    t0: f64,
    u0: f64,
    u1: f64,
    frame: usize,
}

impl<P: Profile> Samples<'_, P> {
    fn pre_onset(&self, t: f64) -> f64 {
        let nb = usize::from(self.ch.n_bins);
        let centre = |i| bin_centre::<P>(i, nb);
        let resid = |i: usize| f64::from(self.ch.bins[i]) / 8.0 - self.line.at(centre(i));
        let base = self.line.at(t);
        if t <= centre(0) {
            return base + resid(0);
        }
        if t >= centre(nb - 1) {
            return base + resid(nb - 1);
        }
        let i = (1..nb).find(|&i| t < centre(i)).unwrap_or(nb - 1);
        let (t0, t1) = (centre(i - 1), centre(i));
        let (r0, r1) = (resid(i - 1), resid(i));
        base + r0 + (t - t0) * (r1 - r0) / (t1 - t0)
    }

    fn post_onset(&self, t: f64) -> f64 {
        let base = self.line.at(t);
        if t <= 0.0 {
            return base;
        }
        let intervals = usize::from(P::KNOTS) + 1;
        #[allow(clippy::cast_precision_loss)]
        let s = (libm::log(t + self.t0) - self.u0) / (self.u1 - self.u0) * intervals as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = (libm::floor(s).max(0.0) as usize).min(intervals - 1);
        #[allow(clippy::cast_precision_loss)]
        let x = s - i as f64;
        let kind = match i {
            0 => 0,
            1 => 1,
            _ if i == intervals - 1 => 4,
            _ if i == intervals - 2 => 3,
            _ => 2,
        };
        let mut acc = 0.0;
        for (m, c) in BASIS_C12[kind].iter().enumerate() {
            let v = ((f64::from(c[3]) * x + f64::from(c[2])) * x + f64::from(c[1])) * x + f64::from(c[0]);
            acc += v / 12.0 * self.w[i + m];
        }
        base + acc
    }
}

impl<P: Profile> Iterator for Samples<'_, P> {
    type Item = f64;

    fn next(&mut self) -> Option<f64> {
        if self.frame >= self.ch.n_frames() {
            return None;
        }
        let onset = usize::from(self.ch.n_bins) * usize::from(P::BIN_FRAMES);
        #[allow(clippy::cast_precision_loss)]
        let t = P::frame_time_s(self.frame as f64 - onset as f64);
        let v = if self.frame < onset { self.pre_onset(t) } else { self.post_onset(t) };
        self.frame += 1;
        Some(v)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.ch.n_frames() - self.frame;
        (left, Some(left))
    }
}

impl<P: Profile> ExactSizeIterator for Samples<'_, P> {}

#[cfg(feature = "alloc")]
mod owned {
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use super::{DecodeError, Decoder};
    use crate::header::{Header, Layout};
    use crate::{East30K12, East30K16, Profile};

    /// A decoded record (feature `alloc`).
    #[derive(Debug, Clone, PartialEq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    #[non_exhaustive]
    pub struct Decoded {
        /// Compact (version byte) or extended (profile id) header.
        pub layout: Layout,
        /// The profile's [`NAME`](Profile::NAME).
        pub profile: String,
        /// The sensor's channel count `N`.
        pub n_channels: u16,
        /// Reference excitation, ADC counts.
        pub e_ref: u16,
        /// Baseline bins before onset.
        pub n_bins: u8,
        /// Period of the sampling grid, microseconds.
        pub frame_period_us: u32,
        /// Frame index of heater onset.
        pub onset_frame: usize,
        /// One entry per live channel, in channel order.
        pub channels: Vec<DecodedChannel>,
    }

    /// One decoded channel (feature `alloc`).
    #[derive(Debug, Clone, PartialEq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    #[non_exhaustive]
    pub struct DecodedChannel {
        /// Channel number, 1-based.
        pub channel: u16,
        /// Baseline bin means, 1/8 counts at `E_ref`.
        pub bins: Vec<i32>,
        /// Quantized rise coefficients.
        pub coefs: Vec<i32>,
        /// Reconstruction in counts at `E_ref`, one per frame; see
        /// [`Decoded::times`].
        pub samples: Vec<f64>,
    }

    impl Decoded {
        /// Time of each sample in seconds from heater onset.
        #[must_use]
        #[allow(clippy::cast_precision_loss)]
        pub fn times(&self) -> Vec<f64> {
            let n = self.channels.first().map_or(0, |c| c.samples.len());
            (0..n).map(|i| (i as f64 - self.onset_frame as f64) * f64::from(self.frame_period_us) / 1e6).collect()
        }

        /// The channel numbered `c`, if it is in the record.
        #[must_use]
        pub fn channel(&self, c: u16) -> Option<&DecodedChannel> {
            self.channels.iter().find(|ch| ch.channel == c)
        }
    }

    impl<P: Profile> Decoder<'_, P> {
        /// Decode every channel into owned buffers (feature `alloc`).
        #[must_use]
        pub fn to_decoded(&self) -> Decoded {
            let h = self.header();
            Decoded {
                layout: h.layout(),
                profile: P::NAME.to_string(),
                n_channels: h.n_channels(),
                e_ref: h.e_ref(),
                n_bins: h.n_bins(),
                frame_period_us: P::FRAME_PERIOD_US,
                onset_frame: self.onset_frame(),
                channels: self
                    .channels()
                    .map(|ch| DecodedChannel {
                        channel: ch.channel(),
                        bins: ch.bins().to_vec(),
                        coefs: ch.coefs().to_vec(),
                        samples: ch.samples().collect(),
                    })
                    .collect(),
            }
        }
    }

    /// Decode a record of either built-in profile ([`East30K12`] or
    /// [`East30K16`], compact or extended header), whatever its channel
    /// count (feature `alloc`). For a custom profile use
    /// [`Decoder::<P>::new`](Decoder::new) and
    /// [`to_decoded`](Decoder::to_decoded).
    ///
    /// ```
    /// let record: &[u8] = &[0x53, 0, 0, 0x61, 0xa8, 12];
    /// let dec = sfk1::decode(record)?;
    /// assert_eq!((dec.e_ref, dec.n_channels), (25_000, 12));
    /// assert!(dec.channels.is_empty());
    /// # Ok::<(), sfk1::DecodeError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Decoder::new`]; [`DecodeError::WrongProfile`] if the record is
    /// not East30.
    pub fn decode(record: &[u8]) -> Result<Decoded, DecodeError> {
        let layout = Header::parse(record)?.layout();
        if super::profile_matches::<East30K12>(layout) {
            Ok(Decoder::<East30K12>::new(record)?.to_decoded())
        } else if super::profile_matches::<East30K16>(layout) {
            Ok(Decoder::<East30K16>::new(record)?.to_decoded())
        } else {
            Err(DecodeError::WrongProfile { found: layout, expected: "East30" })
        }
    }
}

#[cfg(feature = "alloc")]
pub use owned::{decode, Decoded, DecodedChannel};
