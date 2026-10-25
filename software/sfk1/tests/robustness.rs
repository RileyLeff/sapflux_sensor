//! The encoder and decoder never panic and never produce an invalid record,
//! whatever they are fed; channel counts other than 12 round-trip through
//! the extended header. Run in debug too: overflow checks are on there.

use proptest::prelude::*;
#[cfg(feature = "decode")]
use sfk1::{DecodeError, Decoder, Layout};
use sfk1::{East30K12, East30K16, EncodeError, Encoder, Encoder12, Encoder16, Profile, MAX_RECORD_LEN};

/// xorshift64*, so a whole window comes from one shrinkable seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn i16(&mut self) -> i16 {
        (self.next() >> 48) as u16 as i16
    }
}

/// How a test window's counts are drawn.
#[derive(Debug, Clone, Copy)]
enum Signal {
    /// Uniform over the whole `i16` range, every channel and frame.
    Noise,
    /// Every value `i16::MIN` or `i16::MAX`.
    Rails,
    /// A plausible pulse with a step in excitation and some glitches.
    Pulse,
    /// Excitation 0 or negative (a dead reference).
    DeadExcitation,
}

/// One frame: excitation and `N` thermistors.
fn frame<const N: usize>(rng: &mut Rng, sig: Signal, n: u16, onset: u16) -> (i16, [i16; N]) {
    let mut t = [0i16; N];
    match sig {
        Signal::Noise => {
            t.iter_mut().for_each(|v| *v = rng.i16());
            (rng.i16(), t)
        }
        Signal::Rails => {
            let mut rail = || if rng.next() & 1 == 0 { i16::MIN } else { i16::MAX };
            t.iter_mut().for_each(|v| *v = rail());
            (rail(), t)
        }
        Signal::Pulse => {
            for (c, v) in t.iter_mut().enumerate() {
                let rise = if n >= onset { i32::from((n - onset).min(2000) / 10) } else { 0 };
                let x = 9_000 + 500 * (c % 24) as i32 + rise + (rng.next() % 5) as i32 - 2;
                *v = x as i16;
            }
            if rng.next().is_multiple_of(997) {
                t[(rng.next() % N as u64) as usize] = rng.i16();
            }
            (if n > 3000 { 24_000 } else { 24_900 }, t)
        }
        Signal::DeadExcitation => {
            t.iter_mut().for_each(|v| *v = rng.i16());
            (-((rng.next() % 3) as i16), t)
        }
    }
}

/// Which frames of a window are missing.
fn missing_frames(seed: u64, n_frames: u16, onset: u16, p_missing: u8, dead_pre: bool) -> Vec<bool> {
    let mut rng = Rng(seed ^ 0x9e37_79b9_7f4a_7c15 | 1);
    (0..n_frames).map(|n| (dead_pre && n < onset) || (rng.next() % 100) < u64::from(p_missing)).collect()
}

/// Encode a whole window; `None` when the encoder reports `NoBaseline`.
fn encode<const N: usize, P: Profile>(
    mask: &[u8],
    nb: u8,
    seed: u64,
    sig: Signal,
    missing: &[bool],
) -> Option<Vec<u8>> {
    let mut enc = Encoder::<N, P>::new(mask, nb).unwrap();
    let mut rng = Rng(seed | 1);
    let onset = enc.onset_frame();
    assert_eq!(missing.len(), usize::from(enc.frames_expected()));
    for (n, &gone) in (0..).zip(missing) {
        // draw the frame either way so the signal does not depend on gaps
        let (exc, therm) = frame::<N>(&mut rng, sig, n, onset);
        if gone {
            enc.push_missing().unwrap();
        } else {
            enc.push(exc, &therm).unwrap();
        }
    }
    assert_eq!(enc.push_missing(), Err(EncodeError::WindowFull));
    let mut out = vec![0u8; Encoder::<N, P>::MAX_RECORD_LEN];
    match enc.finish(&mut out) {
        Ok(done) => {
            assert!(done.len <= Encoder::<N, P>::MAX_RECORD_LEN);
            out.truncate(done.len);
            Some(out)
        }
        Err(EncodeError::NoBaseline) => None,
        Err(e) => panic!("{e}"),
    }
}

fn run<const N: usize, P: Profile>(mask: &[u8], nb: u8, seed: u64, sig: Signal, p_missing: u8, dead_pre: bool) {
    let onset = u16::from(nb) * P::BIN_FRAMES;
    let missing = missing_frames(seed, onset + P::POST_FRAMES, onset, p_missing, dead_pre);
    let Some(rec) = encode::<N, P>(mask, nb, seed, sig, &missing) else {
        assert!(dead_pre || p_missing == 100);
        return;
    };
    #[cfg(feature = "decode")]
    {
        let dec = Decoder::<P>::new(&rec).expect("encoder output must decode");
        assert_eq!(usize::from(dec.header().n_channels()), N);
        assert_eq!(dec.header().mask(), mask);
        assert_eq!(dec.header().n_bins(), nb);
        for ch in dec.channels() {
            assert!(ch.samples().all(f64::is_finite));
        }
    }
    #[cfg(not(feature = "decode"))]
    let _ = rec;
}

fn signal() -> impl Strategy<Value = Signal> {
    prop_oneof![Just(Signal::Noise), Just(Signal::Rails), Just(Signal::Pulse), Just(Signal::DeadExcitation)]
}

fn p_missing() -> impl Strategy<Value = u8> {
    prop_oneof![Just(0u8), 0u8..=10, Just(60u8), Just(100u8)]
}

/// A big-endian live mask for `n` channels from random bits.
fn mask_for(n: usize, bits: u64) -> Vec<u8> {
    let bytes = n.div_ceil(8);
    let mut m = vec![0u8; bytes];
    for p in 0..n {
        if (bits.rotate_left(p as u32) ^ (p as u64 * 0x9e37)) & 1 == 1 {
            m[bytes - 1 - p / 8] |= 1 << (p % 8);
        }
    }
    m
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn encoder_never_panics(
        mask in 0u16..(1 << 12),
        nb in 2u8..=12,
        k16 in any::<bool>(),
        seed in any::<u64>(),
        sig in signal(),
        p_missing in p_missing(),
        dead_pre in proptest::bool::weighted(0.1),
    ) {
        let mask = mask.to_be_bytes();
        if k16 {
            run::<12, East30K16>(&mask, nb, seed, sig, p_missing, dead_pre);
        } else {
            run::<12, East30K12>(&mask, nb, seed, sig, p_missing, dead_pre);
        }
    }

    #[test]
    fn encoder_never_panics_extended(
        bits in any::<u64>(),
        nb in 2u8..=12,
        seed in any::<u64>(),
        sig in signal(),
        p_missing in p_missing(),
    ) {
        run::<17, East30K16>(&mask_for(17, bits), nb, seed, sig, p_missing, false);
    }

    #[test]
    #[cfg(feature = "decode")]
    fn decoder_never_panics_on_bytes(data in proptest::collection::vec(any::<u8>(), 0..400), extended in any::<bool>()) {
        let mut data = data;
        if extended && !data.is_empty() {
            data[0] = sfk1::EXTENDED_VERSION;
        }
        if let Ok(dec) = Decoder::<East30K12>::new(&data) {
            for ch in dec.channels() {
                let mut buf = vec![0.0; ch.n_frames()];
                ch.reconstruct(&mut buf).unwrap();
            }
        }
    }

    #[test]
    #[cfg(feature = "decode")]
    fn decoder_never_panics_on_mutated_records(
        flips in proptest::collection::vec((0usize..2000, 0u8..8), 1..6),
        cut in 0usize..400,
        extended in any::<bool>(),
    ) {
        let mut rec = if extended { pulse_record_extended() } else { pulse_record() };
        for (i, b) in flips {
            let n = rec.len();
            rec[i % n] ^= 1 << b;
        }
        rec.truncate(rec.len().saturating_sub(cut % 4));
        if let Ok(dec) = Decoder::<East30K12>::new(&rec) {
            prop_assert_eq!(dec.channels().count(), dec.header().n_live());
            for ch in dec.channels() {
                prop_assert!(ch.samples().all(f64::is_finite));
            }
        }
    }
}

/// Encode the window of `N` channels, then each channel alone with `N = 1`,
/// and check the channel integers and curves agree: channels are encoded
/// independently, whatever the header.
#[cfg(feature = "decode")]
fn channels_are_independent<const N: usize>(bits: u64, nb: u8, seed: u64, p_missing: u8) {
    let mask = mask_for(N, bits);
    let onset = u16::from(nb) * East30K12::BIN_FRAMES;
    let missing = missing_frames(seed, onset + East30K12::POST_FRAMES, onset, p_missing, false);
    let rec = encode::<N, East30K12>(&mask, nb, seed, Signal::Pulse, &missing).unwrap();
    let dec = Decoder::<East30K12>::new(&rec).unwrap();
    let h = dec.header();
    let want_layout =
        if N == 12 { Layout::Compact { version: 0x53 } } else { Layout::Extended { profile_id: East30K12::ID } };
    assert_eq!(h.layout(), want_layout);
    assert_eq!(usize::from(h.n_channels()), N);
    assert_eq!(h.byte_len(), Encoder::<N, East30K12>::HEADER_LEN);
    let live: Vec<u16> = h.live_channels().collect();
    assert_eq!(dec.channels().map(|c| c.channel()).collect::<Vec<_>>(), live);

    // re-encode a few live channels alone: same frames, one thermistor
    for ch in dec.channels().take(3) {
        let c = usize::from(ch.channel()) - 1;
        let mut enc = Encoder::<1, East30K12>::new(&[1], nb).unwrap();
        let mut rng = Rng(seed | 1);
        for (n, &gone) in (0..).zip(&missing) {
            let (exc, therm) = frame::<N>(&mut rng, Signal::Pulse, n, onset);
            if gone {
                enc.push_missing().unwrap();
            } else {
                enc.push(exc, &[therm[c]]).unwrap();
            }
        }
        let mut out = [0u8; 512];
        let len = enc.finish(&mut out).unwrap().len;
        let one = &out[..len];
        let one = Decoder::<East30K12>::new(one).unwrap();
        assert_eq!(one.header().n_channels(), 1);
        assert_eq!(one.header().e_ref(), h.e_ref());
        let alone = one.channels().next().unwrap();
        assert_eq!((alone.bins(), alone.coefs()), (ch.bins(), ch.coefs()), "channel {}", c + 1);
        assert!(alone.samples().eq(ch.samples()));
    }
}

#[cfg(feature = "decode")]
proptest! {
    #![proptest_config(ProptestConfig { cases: 6, ..ProptestConfig::default() })]

    #[test]
    fn round_trip_n1(bits in any::<u64>(), nb in 2u8..=12, seed in any::<u64>(), p in 0u8..=5) {
        channels_are_independent::<1>(bits | 1, nb, seed, p);
    }

    #[test]
    fn round_trip_n12(bits in any::<u64>(), nb in 2u8..=12, seed in any::<u64>(), p in 0u8..=5) {
        channels_are_independent::<12>(bits | 1, nb, seed, p);
    }

    #[test]
    fn round_trip_n17(bits in any::<u64>(), nb in 2u8..=12, seed in any::<u64>(), p in 0u8..=5) {
        channels_are_independent::<17>(bits | 1, nb, seed, p);
    }

    #[test]
    fn round_trip_n64(bits in any::<u64>(), nb in 2u8..=4, seed in any::<u64>(), p in 0u8..=5) {
        channels_are_independent::<64>(bits | 1, nb, seed, p);
    }
}

#[cfg(feature = "decode")]
fn pulse_record() -> Vec<u8> {
    let missing = vec![false; 1500 + 4526];
    encode::<12, East30K12>(&[0x0f, 0xff], 12, 7, Signal::Pulse, &missing).unwrap()
}

#[cfg(feature = "decode")]
fn pulse_record_extended() -> Vec<u8> {
    let missing = vec![false; 1500 + 4526];
    encode::<20, East30K12>(&[0x0f, 0xff, 0xf0], 12, 7, Signal::Pulse, &missing).unwrap()
}

#[test]
fn constructor_errors() {
    assert_eq!(
        Encoder12::new(&[0x10, 0], 12).unwrap_err(),
        EncodeError::InvalidLiveMask { channel: 13, n_channels: 12 }
    );
    assert_eq!(Encoder12::new(&[0], 12).unwrap_err(), EncodeError::MaskLength { got: 1, expected: 2, n_channels: 12 });
    assert_eq!(
        Encoder::<20, East30K12>::new(&[0x10, 0, 0], 12).unwrap_err(),
        EncodeError::InvalidLiveMask { channel: 21, n_channels: 20 }
    );
    assert_eq!(Encoder16::new(&[0, 1], 1).unwrap_err(), EncodeError::InvalidBinCount { n_bins: 1, max: 12 });
    assert_eq!(Encoder16::new(&[0, 1], 13).unwrap_err(), EncodeError::InvalidBinCount { n_bins: 13, max: 12 });
    // the messages say what is wrong
    let msg = Encoder12::new(&[0x10, 0], 12).unwrap_err().to_string();
    assert_eq!(msg, "live mask has channel 13 set but N = 12");
    // const-evaluable, so firmware can build one in a static
    const ENC: Result<Encoder12, EncodeError> = Encoder12::new(&[0, 0b11], 12);
    assert!(ENC.is_ok());
}

#[test]
fn record_bounds() {
    assert_eq!(MAX_RECORD_LEN, 2090);
    assert_eq!(Encoder12::HEADER_LEN, 6);
    assert_eq!(Encoder::<1, East30K12>::HEADER_LEN, 11);
    assert_eq!(Encoder::<17, East30K12>::HEADER_LEN, 13);
    println!(
        "size_of Encoder<N, East30K12>: N=1 {} B, N=12 {} B, N=64 {} B, N=256 {} B; Encoder16 {} B",
        core::mem::size_of::<Encoder<1, East30K12>>(),
        core::mem::size_of::<Encoder12>(),
        core::mem::size_of::<Encoder<64, East30K12>>(),
        core::mem::size_of::<Encoder<256, East30K12>>(),
        core::mem::size_of::<Encoder16>(),
    );
}

#[test]
fn incomplete_window_and_reset() {
    let mut enc = Encoder12::new(&[0, 1], 2).unwrap();
    let mut t = [0i16; 12];
    t[0] = 10_000;
    for _ in 0..100 {
        enc.push(20_000, &t).unwrap();
    }
    let mut out = [0u8; 64];
    assert_eq!(enc.finish(&mut out), Err(EncodeError::Incomplete { pushed: 100, expected: 4776 }));
    enc.reset();
    assert_eq!(enc.frames_pushed(), 0);
    for _ in 0..enc.frames_expected() {
        enc.push(20_000, &t).unwrap();
    }
    let done = enc.finish(&mut out).unwrap();
    assert_eq!(done.e_ref, 20_000);
    #[cfg(feature = "decode")]
    flat_record_decodes(&out[..done.len]);
}

#[cfg(feature = "decode")]
fn flat_record_decodes(rec: &[u8]) {
    // a flat channel at half the excitation: 10_000 counts everywhere
    let dec = Decoder::<East30K12>::new(rec).unwrap();
    let ch = dec.channels().next().unwrap();
    assert_eq!(ch.bins(), &[80_000, 80_000]);
    assert!(ch.coefs().iter().all(|&c| c == 0));
    assert!(ch.samples().all(|v| (v - 10_000.0).abs() < 1e-6));
}

#[test]
#[cfg(feature = "decode")]
fn extended_header_layout() {
    // 20 channels, channels 1, 12 and 20 live
    let mask = [0x08, 0x08, 0x01];
    let mut enc = Encoder::<20, East30K12>::new(&mask, 2).unwrap();
    let mut t = [0i16; 20];
    t[0] = 10_000;
    t[11] = 5_000;
    t[19] = 2_500;
    for _ in 0..enc.frames_expected() {
        enc.push(20_000, &t).unwrap();
    }
    let mut out = [0u8; 256];
    let len = enc.finish(&mut out).unwrap().len;
    let rec = &out[..len];
    let id = East30K12::ID.to_be_bytes();
    assert_eq!(&rec[..13], &[0x5f, id[0], id[1], id[2], id[3], 0, 20, 0x08, 0x08, 0x01, 0x4e, 0x20, 2]);

    let h = sfk1::Header::parse(rec).unwrap();
    assert_eq!(h.layout(), Layout::Extended { profile_id: East30K12::ID });
    assert_eq!((h.n_channels(), h.n_live(), h.e_ref(), h.n_bins(), h.byte_len()), (20, 3, 20_000, 2, 13));
    assert_eq!(h.live_channels().collect::<Vec<_>>(), [1, 12, 20]);
    assert!(h.is_live(12) && !h.is_live(13) && !h.is_live(0) && !h.is_live(21));

    let dec = Decoder::<East30K12>::new(rec).unwrap();
    let levels: Vec<(u16, f64)> = dec.channels().map(|c| (c.channel(), c.samples().nth(3000).unwrap())).collect();
    assert_eq!(levels.len(), 3);
    for ((c, v), want) in levels.iter().zip([10_000.0, 5_000.0, 2_500.0]) {
        assert!((v - want).abs() < 1e-6, "channel {c}: {v}");
    }

    // the 16-knot decoder and a bit above N are refused
    assert!(matches!(Decoder::<East30K16>::new(rec), Err(DecodeError::WrongProfile { .. })));
    let mut bad = rec.to_vec();
    bad[7] |= 0x10;
    assert_eq!(sfk1::Header::parse(&bad).unwrap_err(), DecodeError::InvalidLiveMask { channel: 21, n_channels: 20 });
    bad[5..7].copy_from_slice(&[0, 0]);
    assert_eq!(sfk1::Header::parse(&bad).unwrap_err(), DecodeError::InvalidChannelCount(0));

    #[cfg(feature = "alloc")]
    {
        let owned = sfk1::decode(rec).unwrap();
        assert_eq!((owned.n_channels, owned.profile.as_str()), (20, East30K12::NAME));
        assert_eq!(owned.channels.iter().map(|c| c.channel).collect::<Vec<_>>(), [1, 12, 20]);
    }
}

#[test]
#[cfg(feature = "decode")]
fn padding_must_be_zero() {
    for (rec, n_live) in [(pulse_record(), 12), (pulse_record_extended(), 16)] {
        let dec = Decoder::<East30K12>::new(&rec).unwrap();
        let mut bad = rec.clone();
        *bad.last_mut().unwrap() ^= 1;
        match Decoder::<East30K12>::new(&bad) {
            Err(DecodeError::NonZeroPadding) => {}
            // no padding: the flip changed a coefficient instead (or truncated it)
            Ok(_) | Err(DecodeError::Truncated) => {}
            Err(e) => panic!("{e}"),
        }
        assert_eq!(dec.header().n_live(), n_live);
    }
}

#[test]
#[cfg(feature = "decode")]
fn buffer_length_is_checked() {
    let rec = pulse_record();
    let dec = Decoder::<East30K12>::new(&rec).unwrap();
    let ch = dec.channels().next().unwrap();
    let mut short = [0.0; 10];
    assert_eq!(ch.reconstruct(&mut short), Err(DecodeError::BufferLength { expected: 6026, got: 10 }));
}
