//! [`Profile`]: everything about a sensor's sampling and its trained tables
//! that the encoder and decoder must agree on.

use core::fmt;

use crate::tables;

/// Fixed-size scratch arrays a [`Profile`] names for the encoder's
/// per-channel state: `[i64; L]` and `[i32; L]` for any `L`.
///
/// Implemented for plain arrays only (sealed); a profile writes e.g.
/// `type Acc = [i64; 16];`.
///
/// ```
/// use sfk1::Array;
/// assert_eq!(<[i64; 16] as Array<i64>>::LEN, 16);
/// ```
pub trait Array<T>: Copy + AsRef<[T]> + AsMut<[T]> + sealed::Sealed {
    /// Number of elements.
    const LEN: usize;
    /// The all-zero array.
    const ZERO: Self;
}

mod sealed {
    pub trait Sealed {}
    impl<const L: usize> Sealed for [i64; L] {}
    impl<const L: usize> Sealed for [i32; L] {}
}

impl<const L: usize> Array<i64> for [i64; L] {
    const LEN: usize = L;
    const ZERO: Self = [0; L];
}

impl<const L: usize> Array<i32> for [i32; L] {
    const LEN: usize = L;
    const ZERO: Self = [0; L];
}

/// A sensor shape and its trained tables: the sampling grid, the log-time
/// spline basis, the KLT matrices and the entropy-coder parameters.
///
/// The channel count is not part of the profile; it is the `N` of
/// [`Encoder<N, P>`](crate::Encoder), so one profile serves any number of
/// thermistors that share the same probe geometry and sample rate.
///
/// Implement it on a unit struct. Items with defaults hold the values of
/// the East30 format (25 Hz, 5 s bins, 181 s after onset); override what
/// your sensor changes. The trained tables (`MQ`, `D`, `K_TABLE`) and
/// `KF_Q28` come from `tools/generate.py --profile` (see the crate docs,
/// [Defining your own profile](crate#defining-your-own-profile)).
///
/// Invariants are checked at compile time as soon as an
/// [`Encoder`](crate::Encoder) or [`Decoder`](crate::Decoder) is built for
/// the profile (the error names the broken rule), and at run time by
/// [`validate`](Self::validate).
///
/// ```
/// use sfk1::{East30K12, Profile};
/// assert_eq!(East30K12::KNOTS, 12);
/// assert_eq!(East30K12::n_coefs(), 16);
/// assert!(East30K12::validate().is_ok());
/// ```
///
/// A broken profile does not build once it is used:
///
/// ```compile_fail
/// use sfk1::{East30K12, Encoder, Profile};
/// struct Bad;
/// impl Profile for Bad {
///     const NAME: &'static str = "bad";
///     type Bins = [i32; 12];
///     const KNOTS: u8 = 12;
///     type Acc = [i64; 15]; // must be KNOTS + 4 = 16
///     const KF_Q28: i64 = East30K12::KF_Q28;
///     const MQ: &'static [i32] = East30K12::MQ;
///     const D: &'static [f64] = East30K12::D;
///     const K_TABLE: &'static [u8] = East30K12::K_TABLE;
/// }
/// let _ = Encoder::<12, Bad>::new(&[0, 1], 12); // error: Acc is not [i64; KNOTS + 4]
/// ```
pub trait Profile: Sized + 'static {
    /// Human-readable name, for logs.
    const NAME: &'static str;

    /// Version byte of the compact 6-byte header (`0x53`/`0x54` for the
    /// East30 profiles), used when `N = 12`. `None` (the default) always
    /// writes the extended header.
    const COMPACT_VERSION: Option<u8> = None;

    /// 32-bit profile id in the extended header; a decoder refuses a record
    /// whose id is not its profile's. Defaults to [`fingerprint`] of the
    /// profile, so any change to a table changes the id.
    const ID: u32 = fingerprint::<Self>();

    /// Frame rate of the fixed sampling grid, Hz.
    const SAMPLE_RATE_HZ: u16 = 25;
    /// Frames per baseline bin (`1..=4096`).
    const BIN_FRAMES: u16 = 125;
    /// Baseline bins a standard window uses (`2..=MAX_BINS`).
    const DEFAULT_BINS: u8 = 12;
    /// The encoder's per-channel bin storage, `[i32; MAX_BINS]`
    /// (`MAX_BINS` in `2..=64`).
    type Bins: Array<i32>;
    /// Frames from heater onset (t = 0) to the end of the window, inclusive
    /// of both (`2..`; `MAX_BINS * BIN_FRAMES + POST_FRAMES <= 65535`).
    const POST_FRAMES: u16 = 4526;
    /// Time offset of the log-time axis `u = ln(t + t0)`, in frames
    /// (`t0 = 2 s` is 50 frames at 25 Hz).
    const T0_FRAMES: u32 = 50;

    /// Interior knots of the cubic B-spline basis in `u` (`3..=60`); there
    /// are `KNOTS + 4` coefficients per channel.
    const KNOTS: u8;
    /// The encoder's per-channel accumulators, `[i64; KNOTS + 4]`.
    type Acc: Array<i64>;
    /// Knot intervals per unit of `log2((n + T0_FRAMES) / T0_FRAMES)`, Q28:
    /// `round(2^28 (KNOTS + 1) ln 2 / (ln(t_end + t0) + 1e-9 - ln t0))`.
    const KF_Q28: i64;
    /// Encoder matrix, row-major `n_coefs x n_coefs`:
    /// `round(M 2^MQ_SHIFT / (192 STEP))`, `M` the KLT-rotated projection.
    const MQ: &'static [i32];
    /// Fixed-point scale of [`MQ`](Self::MQ) (`1..=62`).
    const MQ_SHIFT: u32 = 36;
    /// Decoder matrix, row-major `n_coefs x n_coefs`: post-onset curve is
    /// `B D (STEP ci)`.
    const D: &'static [f64];
    /// Coefficient quantization step, counts.
    const STEP: f64 = 3.0;

    /// Rice parameter per coefficient, `n_coefs` entries.
    const K_TABLE: &'static [u8];
    /// Bits of the per-channel Rice offset field (`1..=4`).
    const OFFSET_BITS: u32 = 2;
    /// The offset a field value of 0 stands for (East30: -1, so 2 bits give
    /// -1..=2).
    const OFFSET_MIN: i32 = -1;
    /// Unary quotients this long or longer are escaped (`1..=31`).
    const ESCAPE_Q: u32 = 24;
    /// Raw zigzag bits after an escape (`B0_BITS + 1..=24`); coefficients
    /// are clamped to this range.
    const ESCAPE_BITS: u32 = 20;
    /// Bits of the first baseline bin (`1..=23`), unsigned 1/8 counts.
    const B0_BITS: u32 = 19;

    /// Coefficients per channel, `KNOTS + 4`.
    #[must_use]
    fn n_coefs() -> usize {
        usize::from(Self::KNOTS) + 4
    }

    /// Most baseline bins the encoder stores (the length of
    /// [`Bins`](Self::Bins)).
    #[must_use]
    fn max_bins() -> u8 {
        u8::try_from(<Self::Bins as Array<i32>>::LEN).unwrap_or(u8::MAX)
    }

    /// Check the profile's invariants at run time (the same rules the
    /// compile-time check enforces).
    ///
    /// # Errors
    ///
    /// The first [`ProfileError`] found.
    fn validate() -> Result<(), ProfileError> {
        check::<Self>()
    }
}

/// A [`Profile`] invariant that does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum ProfileError {
    /// `KNOTS` is outside `3..=60`.
    Knots,
    /// `Acc` is not `[i64; KNOTS + 4]`.
    AccLength,
    /// `MQ` does not have `n_coefs^2` entries.
    MqShape,
    /// `D` does not have `n_coefs^2` entries.
    DShape,
    /// `K_TABLE` does not have `n_coefs` entries.
    KTableShape,
    /// A Rice parameter `K_TABLE[j] + offset` can exceed 31.
    RiceParameter,
    /// `OFFSET_BITS` outside `1..=4`.
    OffsetBits,
    /// `MAX_BINS` outside `2..=64` or `DEFAULT_BINS` outside `2..=MAX_BINS`.
    Bins,
    /// `BIN_FRAMES` outside `1..=4096`.
    BinFrames,
    /// The window (`MAX_BINS * BIN_FRAMES + POST_FRAMES`) exceeds 65535
    /// frames, or `POST_FRAMES < 2`.
    WindowLength,
    /// `T0_FRAMES` outside `1..=65536`, or `SAMPLE_RATE_HZ` is 0.
    TimeAxis,
    /// `KF_Q28` is not positive.
    KnotScale,
    /// `MQ_SHIFT` outside `1..=62`.
    MqShift,
    /// `B0_BITS` outside `1..=23`, or `ESCAPE_BITS` outside
    /// `B0_BITS + 1..=24`, or `ESCAPE_Q` outside `1..=31`.
    FieldWidths,
    /// `STEP` is not a positive finite number.
    Step,
}

impl ProfileError {
    /// What is wrong and how to fix it.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Knots => "Profile::KNOTS must be in 3..=60",
            Self::AccLength => "Profile::Acc must be [i64; KNOTS + 4] (e.g. [i64; 16] for 12 knots)",
            Self::MqShape => "Profile::MQ must have (KNOTS + 4)^2 entries, row-major",
            Self::DShape => "Profile::D must have (KNOTS + 4)^2 entries, row-major",
            Self::KTableShape => "Profile::K_TABLE must have KNOTS + 4 entries",
            Self::RiceParameter => "Profile::K_TABLE[j] + OFFSET_MIN + 2^OFFSET_BITS - 1 must be <= 31 for every j",
            Self::OffsetBits => "Profile::OFFSET_BITS must be in 1..=4",
            Self::Bins => {
                "Profile::Bins must be [i32; MAX_BINS] with MAX_BINS in 2..=64, and DEFAULT_BINS in 2..=MAX_BINS"
            }
            Self::BinFrames => "Profile::BIN_FRAMES must be in 1..=4096",
            Self::WindowLength => "Profile: MAX_BINS * BIN_FRAMES + POST_FRAMES must be <= 65535 and POST_FRAMES >= 2",
            Self::TimeAxis => "Profile::T0_FRAMES must be in 1..=65536 and SAMPLE_RATE_HZ must be > 0",
            Self::KnotScale => "Profile::KF_Q28 must be > 0 (see its docs for the formula)",
            Self::MqShift => "Profile::MQ_SHIFT must be in 1..=62",
            Self::FieldWidths => "Profile: need 1 <= B0_BITS <= 23, B0_BITS < ESCAPE_BITS <= 24, 1 <= ESCAPE_Q <= 31",
            Self::Step => "Profile::STEP must be positive and finite",
        }
    }
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl core::error::Error for ProfileError {}

/// Check `P`'s invariants; `const`, so it also runs at compile time.
pub(crate) const fn check<P: Profile>() -> Result<(), ProfileError> {
    let nc = P::KNOTS as usize + 4;
    if P::KNOTS < 3 || P::KNOTS > 60 {
        return Err(ProfileError::Knots);
    }
    if <P::Acc as Array<i64>>::LEN != nc {
        return Err(ProfileError::AccLength);
    }
    if P::MQ.len() != nc * nc {
        return Err(ProfileError::MqShape);
    }
    if P::D.len() != nc * nc {
        return Err(ProfileError::DShape);
    }
    if P::K_TABLE.len() != nc {
        return Err(ProfileError::KTableShape);
    }
    if P::OFFSET_BITS < 1 || P::OFFSET_BITS > 4 {
        return Err(ProfileError::OffsetBits);
    }
    let o_max = P::OFFSET_MIN as i64 + (1 << P::OFFSET_BITS) - 1;
    let mut j = 0;
    while j < nc {
        if P::K_TABLE[j] as i64 + o_max > 31 {
            return Err(ProfileError::RiceParameter);
        }
        j += 1;
    }
    let max_bins = <P::Bins as Array<i32>>::LEN;
    if max_bins < 2 || max_bins > 64 || P::DEFAULT_BINS < 2 || P::DEFAULT_BINS as usize > max_bins {
        return Err(ProfileError::Bins);
    }
    if P::BIN_FRAMES < 1 || P::BIN_FRAMES > 4096 {
        return Err(ProfileError::BinFrames);
    }
    if P::POST_FRAMES < 2 || max_bins * P::BIN_FRAMES as usize + P::POST_FRAMES as usize > 65_535 {
        return Err(ProfileError::WindowLength);
    }
    if P::T0_FRAMES < 1 || P::T0_FRAMES > 65_536 || P::SAMPLE_RATE_HZ == 0 {
        return Err(ProfileError::TimeAxis);
    }
    if P::KF_Q28 <= 0 {
        return Err(ProfileError::KnotScale);
    }
    if P::MQ_SHIFT < 1 || P::MQ_SHIFT > 62 {
        return Err(ProfileError::MqShift);
    }
    if P::B0_BITS < 1
        || P::B0_BITS > 23
        || P::ESCAPE_BITS <= P::B0_BITS
        || P::ESCAPE_BITS > 24
        || P::ESCAPE_Q < 1
        || P::ESCAPE_Q > 31
    {
        return Err(ProfileError::FieldWidths);
    }
    if !(P::STEP > 0.0 && P::STEP < f64::INFINITY) {
        return Err(ProfileError::Step);
    }
    Ok(())
}

/// Compile-time form of [`check`]: a broken profile fails the build with
/// the rule it broke.
pub(crate) const fn assert_valid<P: Profile>() {
    if let Err(e) = check::<P>() {
        panic!("{}", e.message());
    }
}

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

const fn fnv(mut h: u32, bytes: &[u8]) -> u32 {
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}

/// FNV-1a (32-bit) over the big-endian bytes of everything in `P` that
/// changes the bitstream or its meaning, in this order: `SAMPLE_RATE_HZ`
/// (u16), `BIN_FRAMES` (u16), `POST_FRAMES` (u16), `T0_FRAMES` (u32),
/// `KNOTS` (u8), `KF_Q28` (i64), `MQ_SHIFT`, `OFFSET_BITS`, `ESCAPE_Q`,
/// `ESCAPE_BITS`, `B0_BITS` (u32 each), `OFFSET_MIN` (i32), `STEP` (f64
/// bits), `K_TABLE` (u8 each), `MQ` (i32 each), `D` (f64 bits each).
///
/// ```
/// use sfk1::{fingerprint, East30K12, East30K16, Profile};
/// assert_eq!(East30K12::ID, fingerprint::<East30K12>());
/// assert_ne!(East30K12::ID, East30K16::ID);
/// ```
#[must_use]
pub const fn fingerprint<P: Profile>() -> u32 {
    let mut h = FNV_OFFSET;
    h = fnv(h, &P::SAMPLE_RATE_HZ.to_be_bytes());
    h = fnv(h, &P::BIN_FRAMES.to_be_bytes());
    h = fnv(h, &P::POST_FRAMES.to_be_bytes());
    h = fnv(h, &P::T0_FRAMES.to_be_bytes());
    h = fnv(h, &[P::KNOTS]);
    h = fnv(h, &P::KF_Q28.to_be_bytes());
    h = fnv(h, &P::MQ_SHIFT.to_be_bytes());
    h = fnv(h, &P::OFFSET_BITS.to_be_bytes());
    h = fnv(h, &P::ESCAPE_Q.to_be_bytes());
    h = fnv(h, &P::ESCAPE_BITS.to_be_bytes());
    h = fnv(h, &P::B0_BITS.to_be_bytes());
    h = fnv(h, &P::OFFSET_MIN.to_be_bytes());
    h = fnv(h, &P::STEP.to_bits().to_be_bytes());
    h = fnv(h, P::K_TABLE);
    let mut i = 0;
    while i < P::MQ.len() {
        h = fnv(h, &P::MQ[i].to_be_bytes());
        i += 1;
    }
    let mut i = 0;
    while i < P::D.len() {
        h = fnv(h, &P::D[i].to_bits().to_be_bytes());
        i += 1;
    }
    h
}

/// The East30 sap-flux probe at 25 Hz with 12 interior knots: version
/// byte `0x53`, the recommended default (about 250 B per 12-thermistor
/// pulse).
///
/// ```
/// use sfk1::{East30K12, Profile};
/// assert_eq!(East30K12::COMPACT_VERSION, Some(0x53));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct East30K12;

/// The East30 probe with 16 interior knots: version `0x54`, slightly
/// higher fidelity for about 16 B more per pulse.
///
/// ```
/// use sfk1::{East30K16, Profile};
/// assert_eq!(East30K16::n_coefs(), 20);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct East30K16;

impl Profile for East30K12 {
    const NAME: &'static str = "East30 25 Hz, 12 knots";
    const COMPACT_VERSION: Option<u8> = Some(0x53);
    type Bins = [i32; 12];
    const KNOTS: u8 = 12;
    type Acc = [i64; 16];
    const KF_Q28: i64 = tables::KF_K12;
    const MQ: &'static [i32] = tables::MQ_K12.as_flattened();
    const D: &'static [f64] = tables::D_K12.as_flattened();
    const K_TABLE: &'static [u8] = &tables::K_TABLE_K12;
}

impl Profile for East30K16 {
    const NAME: &'static str = "East30 25 Hz, 16 knots";
    const COMPACT_VERSION: Option<u8> = Some(0x54);
    type Bins = [i32; 12];
    const KNOTS: u8 = 16;
    type Acc = [i64; 20];
    const KF_Q28: i64 = tables::KF_K16;
    const MQ: &'static [i32] = tables::MQ_K16.as_flattened();
    const D: &'static [f64] = tables::D_K16.as_flattened();
    const K_TABLE: &'static [u8] = &tables::K_TABLE_K16;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn east30_is_valid() {
        assert_eq!(East30K12::validate(), Ok(()));
        assert_eq!(East30K16::validate(), Ok(()));
    }

    #[test]
    fn broken_profiles_are_named() {
        struct Bad;
        impl Profile for Bad {
            const NAME: &'static str = "bad";
            type Bins = [i32; 12];
            const KNOTS: u8 = 12;
            type Acc = [i64; 15];
            const KF_Q28: i64 = tables::KF_K12;
            const MQ: &'static [i32] = tables::MQ_K12.as_flattened();
            const D: &'static [f64] = tables::D_K12.as_flattened();
            const K_TABLE: &'static [u8] = &tables::K_TABLE_K12;
        }
        assert_eq!(Bad::validate(), Err(ProfileError::AccLength));
        assert!(ProfileError::AccLength.to_string().contains("[i64; KNOTS + 4]"));
    }
}
