//! The record header: compact (East30, 12 channels) or extended.
//!
//! ```text
//! compact   u8 version (0x53 / 0x54)  u16 mask            u16 E_ref  u8 NB
//! extended  u8 0x5F  u32 profile id  u16 N  ceil(N/8) B mask  u16 E_ref  u8 NB
//! ```
//!
//! All big-endian. The mask is one big-endian integer, bit `c - 1` set for
//! a channel `c` in the record (so the compact mask is the extended mask of
//! `N = 12` rounded up to 16 bits); bits `>= N` must be 0.

use crate::{DecodeError, EXTENDED_VERSION, MAX_DECODE_BINS};

/// Which header a record has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Layout {
    /// The 6-byte East30 header: version byte `0x53` or `0x54`, 12 channels.
    Compact {
        /// The version byte.
        version: u8,
    },
    /// The extended header: a [`Profile::ID`](crate::Profile::ID) and an
    /// explicit channel count.
    Extended {
        /// The profile id.
        profile_id: u32,
    },
}

/// A parsed, validated record header; borrows its live mask from the
/// record.
///
/// ```
/// let rec = [0x53, 0x00, 0x05, 0x61, 0xa8, 12];
/// let h = sfk1::Header::parse(&rec)?;
/// assert_eq!(h.n_channels(), 12);
/// assert_eq!(h.live_channels().collect::<Vec<_>>(), [1, 3]);
/// assert_eq!((h.e_ref(), h.n_bins(), h.byte_len()), (25_000, 12, 6));
/// # Ok::<(), sfk1::DecodeError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header<'a> {
    layout: Layout,
    n_channels: u16,
    mask: &'a [u8],
    e_ref: u16,
    n_bins: u8,
}

struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Truncated);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Whether channel `c` (1-based) is set in a big-endian mask.
pub(crate) fn mask_bit(mask: &[u8], c: usize) -> bool {
    let p = c - 1;
    mask.len().checked_sub(1 + p / 8).and_then(|i| mask.get(i)).is_some_and(|b| b >> (p % 8) & 1 == 1)
}

impl<'a> Header<'a> {
    /// Parse and validate the header at the start of `record`.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Truncated`], [`DecodeError::UnsupportedVersion`],
    /// [`DecodeError::InvalidChannelCount`], [`DecodeError::InvalidLiveMask`]
    /// or [`DecodeError::InvalidBinCount`].
    pub fn parse(record: &'a [u8]) -> Result<Self, DecodeError> {
        let mut r = Bytes(record);
        let version = r.u8()?;
        let (layout, n_channels) = match version {
            0x53 | 0x54 => (Layout::Compact { version }, 12),
            EXTENDED_VERSION => {
                let profile_id = r.u32()?;
                let n = r.u16()?;
                if n == 0 {
                    return Err(DecodeError::InvalidChannelCount(0));
                }
                (Layout::Extended { profile_id }, n)
            }
            v => return Err(DecodeError::UnsupportedVersion(v)),
        };
        let mask_len = if matches!(layout, Layout::Compact { .. }) { 2 } else { usize::from(n_channels).div_ceil(8) };
        let mask = r.take(mask_len)?;
        if let Some(c) = (usize::from(n_channels) + 1..=8 * mask_len).find(|&c| mask_bit(mask, c)) {
            #[allow(clippy::cast_possible_truncation)] // c <= 8 * 8192
            return Err(DecodeError::InvalidLiveMask { channel: c as u32, n_channels });
        }
        let e_ref = r.u16()?;
        let n_bins = r.u8()?;
        if !(2..=MAX_DECODE_BINS).contains(&usize::from(n_bins)) {
            return Err(DecodeError::InvalidBinCount(n_bins));
        }
        Ok(Self { layout, n_channels, mask, e_ref, n_bins })
    }

    /// Compact or extended, with the version byte or profile id.
    #[must_use]
    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// Channel count `N` of the sensor (12 for a compact header).
    #[must_use]
    pub const fn n_channels(&self) -> u16 {
        self.n_channels
    }

    /// The live mask bytes as in the record (big-endian, bit `c - 1` =
    /// channel `c`).
    #[must_use]
    pub const fn mask(&self) -> &'a [u8] {
        self.mask
    }

    /// Whether channel `c` (1-based) is in the record.
    #[must_use]
    pub fn is_live(&self, c: u16) -> bool {
        c >= 1 && c <= self.n_channels && mask_bit(self.mask, usize::from(c))
    }

    /// The channels in the record (1-based), in record order.
    pub fn live_channels(&self) -> impl Iterator<Item = u16> + 'a {
        let (mask, n) = (self.mask, self.n_channels);
        (1..=n).filter(move |&c| mask_bit(mask, usize::from(c)))
    }

    /// Number of channels in the record.
    #[must_use]
    pub fn n_live(&self) -> usize {
        self.mask.iter().map(|b| b.count_ones() as usize).sum()
    }

    /// Reference excitation, ADC counts: decoded channels are in counts at
    /// this excitation.
    #[must_use]
    pub const fn e_ref(&self) -> u16 {
        self.e_ref
    }

    /// Baseline bins before onset.
    #[must_use]
    pub const fn n_bins(&self) -> u8 {
        self.n_bins
    }

    /// Header length in bytes (the bitstream starts here).
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        match self.layout {
            Layout::Compact { .. } => 6,
            Layout::Extended { .. } => 10 + self.mask.len(),
        }
    }
}
