//! SFK1: compression for heat-pulse windows, one record per pulse.
//!
//! A record carries a baseline before a heater pulse and the response after
//! it, from any number of thermistors on a fixed sampling grid. A
//! [`Profile`] sets the grid, the log-time spline basis and the trained
//! coding tables, so one crate serves different probes, spacings, channel
//! counts and frame periods. The bundled East30 profiles are the sap-flux
//! board's: 60 s of baseline plus 181 s from heater onset, 12 thermistors at
//! 25 Hz, about 150 kB raw, in about 250 bytes with no measurable loss of
//! sap-velocity information. The format and the science behind it are in
//! `analysis/compression_research_20261024/README.md` §5; the byte layout is
//! repeated in this crate's README.
//!
//! * [`Encoder<N, P>`](Encoder) runs on the logger: `no_std`, no allocation,
//!   no panics, no floating point. `N` is the thermistor count and `P` the
//!   [`Profile`] (sampling grid and trained tables). Push one frame at a
//!   time ([`Encoder::push`], [`Encoder::push_missing`]); nothing is buffered
//!   beyond a 16-frame ring, and [`Encoder::finish`] writes the record into
//!   a caller buffer.
//! * [`Decoder<P>`](Decoder) (feature `decode`, on by default) parses a
//!   record of any channel count and rebuilds each channel's curve. With
//!   `alloc`, [`decode()`] picks the bundled profile from the header and
//!   returns it all in [`Vec`](alloc::vec::Vec)s. [`Curve`] evaluates a
//!   channel between frames, for front ends that sample channels at fixed
//!   offsets ([sample timing](Curve#sample-timing)).
//!
//! [`Encoder12`] and [`Encoder16`] are the sap-flux board's encoders (12 thermistors,
//! [`East30K12`] / [`East30K16`]). They are bit-exact with the reference's
//! firmware mode, `encoder_spec.encode(w, fixed_point="firmware")`, and the
//! decoder matches the reference decoder; both are checked against golden
//! vectors in `tests/vectors`.
//!
//! # Choosing a profile
//!
//! The bundled East30 profiles trade record size for the fastest flow they
//! reproduce. On simulated 12-thermistor pulses (six at +0.6 cm, six at
//! −0.6 cm, k = 0.0025 cm²/s, 1 s pulse, raw noise 1.53 counts RMS):
//!
//! | profile | knots, t0 | header | bytes at 0–20 cm/h | accurate from −60 to | RMS error at 100 / 150 / 200 cm/h |
//! |---|---|---|---:|---:|---|
//! | [`East30K12`] (default) | 12, 2 s | compact `0x53` | 190 | 70 cm/h | 0.94 / 3.0 / 6.0 |
//! | [`East30K16`] | 16, 2 s | compact `0x54` | 198 | 100 cm/h | 0.24 / 0.74 / 1.9 |
//! | [`East30Wide`] | 20, 2 s | extended | 276 | 150 cm/h | 0.14 / 0.27 / 0.65 |
//! | [`East30Extreme`] | 24, 2 s | extended | 302 | 200 cm/h | 0.14 / 0.17 / 0.29 |
//!
//! Accurate means a full-curve RMS error of at most 0.5 counts and no
//! sample more than 5 counts off the noise-free curve. Pick the smallest
//! profile whose range covers the fastest sap flow you expect; the lossless
//! 25 Hz raw stays on the logger's flash whichever you pick. K12 and K16
//! are trained on −4 to 32 cm/h, Wide on −10 to 150 and Extreme on −20 to
//! 200, so the last two keep records near 260–320 B across their range.
//! [`decode()`] recognises every bundled profile.
//!
//! # Example
//!
//! ```
//! use sfk1::{Encoder12, MAX_RECORD_LEN};
//!
//! // thermistors 1..=5 live (big-endian mask), the default 12 bins (60 s)
//! let mut enc = Encoder12::new(&[0x00, 0b1_1111], 12)?;
//! for n in 0..enc.frames_expected() {
//!     let mut therm = [0i16; 12];
//!     for t in &mut therm[..5] {
//!         *t = 11_000 + if n >= enc.onset_frame() { 40 } else { 0 };
//!     }
//!     if n % 500 == 7 {
//!         enc.push_missing()?; // a dropped ADC conversion
//!     } else {
//!         enc.push(24_900, &therm)?;
//!     }
//! }
//! let mut out = [0u8; MAX_RECORD_LEN];
//! let done = enc.finish(&mut out)?;
//! assert_eq!(done.missing, 13);
//! let record = &out[..done.len];
//!
//! # #[cfg(feature = "decode")] {
//! let dec = sfk1::Decoder::<sfk1::East30K12>::new(record)?;
//! let mut curve = [0.0f64; 6026];
//! for ch in dec.channels() {
//!     ch.reconstruct(&mut curve)?;
//!     assert!((curve[3000] - 11_040.0).abs() < 2.0);
//! }
//! # }
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```
//!
//! # Channel counts and headers
//!
//! A record of `N = 12` channels of a profile with a
//! [`COMPACT_VERSION`](Profile::COMPACT_VERSION) (the East30 profiles) has
//! the original 6-byte header, so board records are unchanged. Any other
//! record has the extended header, which names the profile by its 32-bit
//! [`ID`](Profile::ID) and carries `N` and a `ceil(N / 8)`-byte live mask; a
//! decoder learns `N` from it. Records of several excitation groups (probes
//! with separate excitation lines) are one record per group, each from its
//! own encoder.
//!
//! The encoder's RAM is about 220 bytes per channel plus a small fixed part
//! (East30, 12 bins): `size_of::<Encoder12>()` is 2744 B, `N = 64` is
//! 14 336 B and `N = 256` is 57 152 B, too much for the 144 kB STM32G0B1
//! next to a firmware's buffers and stacks. A large array is better split
//! into groups, one smaller encoder (and record) each.
//!
//! # Defining your own profile
//!
//! A profile is a unit struct implementing [`Profile`]. Override what your
//! sensor changes from the East30 defaults and fill the trained tables from
//! `tools/generate.py --profile NAME --period-us US --post-s S --t0 S --knots K`,
//! which writes `MQ`, `D`, `K_TABLE` and `KF_Q28` for the new grid. Here is a
//! 48-thermistor probe sampled at 10 Hz:
//!
//! ```
//! use sfk1::{Encoder, Profile};
//!
//! mod probe48 {
//!     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/profiles/probe48_10hz.rs"));
//! }
//!
//! struct Probe48;
//! impl Profile for Probe48 {
//!     const NAME: &'static str = "probe48 10 Hz";
//!     const FRAME_PERIOD_US: u32 = 100_000; // 10 Hz
//!     const BIN_FRAMES: u16 = 50; // 5 s bins
//!     type Bins = [i32; 12];
//!     const POST_FRAMES: u16 = 1811; // 181 s
//!     const T0_FRAMES: u32 = 20; // t0 = 2 s
//!     const KNOTS: u8 = 12;
//!     type Acc = [i64; 16];
//!     const KF_Q28: i64 = probe48::KF_Q28;
//!     const MQ: &'static [i32] = probe48::MQ.as_flattened();
//!     const D: &'static [f64] = probe48::D.as_flattened();
//!     const K_TABLE: &'static [u8] = &probe48::K_TABLE;
//! }
//!
//! let mut enc = Encoder::<48, Probe48>::new(&[0xff; 6], 12)?;
//! for _ in 0..enc.frames_expected() {
//!     enc.push(20_000, &[9_000; 48])?;
//! }
//! let mut out = [0u8; 2048];
//! let len = enc.finish(&mut out)?.len;
//! let rec = &out[..len];
//! assert_eq!(rec[0], sfk1::EXTENDED_VERSION);
//!
//! # #[cfg(feature = "decode")] {
//! let dec = sfk1::Decoder::<Probe48>::new(rec)?;
//! assert_eq!((dec.header().n_channels(), dec.header().n_live()), (48, 48));
//! assert!(dec.channels().all(|c| c.samples().all(|v| (v - 9_000.0).abs() < 1e-6)));
//! # }
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```
//!
//! A profile that breaks an invariant (say `Acc` of the wrong length) fails
//! to compile once an encoder or decoder is built for it, with the broken
//! rule in the error; [`Profile::validate`] reports the same at run time.
//!
//! # Features
//!
//! | feature | default | what |
//! |---|---|---|
//! | `decode` | yes | [`Decoder`], [`Header`], [`ChannelCoefs`]; pulls in `libm` |
//! | `alloc` | | [`decode()`], [`Decoded`] |
//! | `std` | | implies `alloc` |
//! | `defmt` | | `defmt::Format` on public types and errors |
//! | `serde` | | `Serialize`/`Deserialize` on [`Layout`], [`Line`] and the decoded types |
//!
//! Firmware uses `default-features = false` (plus `defmt` if wanted) and
//! links only the integer encoder and its profile's tables.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
// fixed-point numerics: short names follow the maths in the module docs
#![allow(clippy::many_single_char_names, clippy::similar_names)]

#[cfg(feature = "alloc")]
extern crate alloc;
// generated profiles name the crate as `sfk1::`, as an outside crate would
extern crate self as sfk1;

mod bits;
#[cfg(feature = "decode")]
mod decode;
mod encoder;
mod fixed;
#[cfg(feature = "decode")]
mod header;
mod profile;
#[allow(clippy::unreadable_literal)] // generated by tools/generate.py
#[rustfmt::skip]
mod tables;
#[allow(clippy::unreadable_literal, clippy::excessive_precision)] // generated by tools/generate.py
#[rustfmt::skip]
mod east30_offset {
    include!("profiles/east30_offset.rs");
}
#[allow(clippy::unreadable_literal, clippy::excessive_precision)] // generated by tools/generate.py
#[rustfmt::skip]
mod east30_wide {
    include!("profiles/east30_wide.rs");
}
#[allow(clippy::unreadable_literal, clippy::excessive_precision)] // generated by tools/generate.py
#[rustfmt::skip]
mod east30_extreme {
    include!("profiles/east30_extreme.rs");
}

#[cfg(feature = "alloc")]
pub use decode::{decode, Decoded, DecodedChannel};
#[cfg(feature = "decode")]
pub use decode::{ChannelCoefs, Channels, Curve, DecodeError, Decoder, Line, Samples, MAX_DECODE_BINS};
/// The East30 probe with 24 knots (`t0` = 2 s), accurate to about
/// 200 cm/h; see [Choosing a profile](crate#choosing-a-profile).
pub use east30_extreme::East30Extreme;
pub use east30_offset::East30Offset;
/// The East30 probe with 20 knots (`t0` = 2 s), accurate to about
/// 150 cm/h; see [Choosing a profile](crate#choosing-a-profile).
pub use east30_wide::East30Wide;
pub use encoder::{EncodeError, Encoder, Encoder12, Encoder16, Finished};
#[cfg(feature = "decode")]
pub use header::{Header, Layout};
pub use profile::{fingerprint, Array, East30K12, East30K16, Profile, ProfileError};

/// Version byte of the extended header (any profile, any channel count).
pub const EXTENDED_VERSION: u8 = 0x5F;

/// Most coefficients per channel any profile may have (60 knots).
pub const MAX_COEFS: usize = 64;

/// A buffer this long holds any record of [`Encoder12`] or [`Encoder16`]
/// (12 live channels, 12 bins, every code escaped): 2090 bytes. Typical
/// records are about 250 B. Other encoders have their own
/// [`Encoder::MAX_RECORD_LEN`].
pub const MAX_RECORD_LEN: usize = Encoder16::MAX_RECORD_LEN;
