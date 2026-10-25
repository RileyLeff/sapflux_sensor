//! The bundled East30 profiles as a ladder of record size against the flow
//! range they reproduce: [`East30K12`] to 70 cm/h, [`East30K16`] to 100,
//! [`East30Wide`] to 150, [`East30Extreme`] to 200. Each is checked on
//! simulated pulses against the noise-free truth.

use sfk1::{East30Extreme, East30K12, East30K16, East30Offset, East30Wide, Encoder, Profile, EXTENDED_VERSION};

/// Diffusivity, cm²/s.
const K: f64 = 0.0025;
/// Thermistors 1-6 at +0.6 cm (downstream), 7-12 at -0.6 cm.
const X: [f64; 2] = [0.6, -0.6];
/// Accurate: full-curve RMS error vs truth and worst single-sample miss,
/// counts, from onset to the end of the window.
const RMSE_MAX: f64 = 0.5;
const WORST_MAX: f64 = 5.0;

/// One frame of a 1 s heat pulse at `x` cm, `v` cm/h: the moving
/// instantaneous line source averaged over the pulse, unscaled.
fn shape(t: f64, x: f64, v: f64) -> f64 {
    let inst = |t: f64| {
        if t <= 0.0 {
            return 0.0;
        }
        (-(x - v / 3600.0 * t).powi(2) / (4.0 * K * t)).exp() / t
    };
    (0..20).map(|i| inst(t - (f64::from(i) + 0.5) / 20.0)).sum::<f64>() / 20.0
}

/// The rise above baseline for each frame from onset, `[+0.6 cm, -0.6 cm]`,
/// scaled to 350 counts at the no-flow 0.6 cm peak.
fn truth(v: f64, n_post: usize) -> Vec<[f64; 2]> {
    let peak = (0..4000).map(|i| shape(f64::from(i) * 0.05, 0.6, 0.0)).fold(0.0, f64::max);
    #[allow(clippy::cast_precision_loss)]
    (0..n_post).map(|i| X.map(|x| 350.0 * shape(i as f64 / 25.0, x, v) / peak)).collect()
}

/// Encode one 12-thermistor pulse at `v` cm/h with 1.5 counts RMS of noise.
fn record<P: Profile>(truth: &[[f64; 2]], seed: u32) -> Vec<u8> {
    let mut enc = Encoder::<12, P>::new(&[0x0f, 0xff], 12).unwrap();
    let mut s = seed | 1;
    let mut noise = || {
        (0..4)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                f64::from(s) / f64::from(u32::MAX) - 0.5
            })
            .sum::<f64>()
            * 1.5
            * 3.0f64.sqrt()
    };
    let on = enc.onset_frame();
    for n in 0..enc.frames_expected() {
        let tr = n.checked_sub(on).map_or([0.0; 2], |i| truth[usize::from(i)]);
        let therm: [i16; 12] = core::array::from_fn(|c| (11_000.0 + tr[usize::from(c >= 6)] + noise()).round() as i16);
        enc.push(25_000, &therm).unwrap();
    }
    let mut out = vec![0u8; Encoder::<12, P>::MAX_RECORD_LEN];
    let len = enc.finish(&mut out).unwrap().len;
    out.truncate(len);
    out
}

/// Record length, full-curve RMS error and worst miss vs truth, counts.
#[cfg(feature = "decode")]
fn errors<P: Profile>(v: f64) -> (usize, f64, f64) {
    let tr = truth(v, usize::from(P::POST_FRAMES));
    let rec = record::<P>(&tr, 0x9e37_79b9);
    let dec = sfk1::Decoder::<P>::new(&rec).unwrap();
    let on = dec.onset_frame();
    let (mut se, mut n, mut worst) = (0.0, 0.0, 0.0f64);
    for ch in dec.channels() {
        let side = usize::from(ch.channel() > 6);
        for (i, y) in ch.samples().skip(on).enumerate() {
            let e = y - 11_000.0 - tr[i][side];
            se += e * e;
            n += 1.0;
            worst = worst.max(e.abs());
        }
    }
    (rec.len(), (se / n).sqrt(), worst)
}

#[test]
fn the_ladder_is_valid_and_its_ids_are_distinct() {
    let ids = [East30K12::ID, East30K16::ID, East30Wide::ID, East30Extreme::ID, East30Offset::ID];
    for (i, a) in ids.iter().enumerate() {
        assert!(ids[i + 1..].iter().all(|b| b != a), "profile id {a:#010x} repeats");
    }
    assert_eq!(East30Wide::validate(), Ok(()));
    assert_eq!(East30Extreme::validate(), Ok(()));
    assert_eq!((East30Wide::KNOTS, East30Wide::T0_FRAMES), (20, 50));
    assert_eq!((East30Extreme::KNOTS, East30Extreme::T0_FRAMES), (24, 50));
    // the same grid as the board's profiles, and no compact version byte
    for (period, bin, post, cv) in [
        (East30Wide::FRAME_PERIOD_US, East30Wide::BIN_FRAMES, East30Wide::POST_FRAMES, East30Wide::COMPACT_VERSION),
        (
            East30Extreme::FRAME_PERIOD_US,
            East30Extreme::BIN_FRAMES,
            East30Extreme::POST_FRAMES,
            East30Extreme::COMPACT_VERSION,
        ),
    ] {
        assert_eq!((period, bin, post), (East30K12::FRAME_PERIOD_US, East30K12::BIN_FRAMES, East30K12::POST_FRAMES));
        assert_eq!(cv, None);
    }
}

#[test]
fn wide_and_extreme_records_use_the_extended_header() {
    let tr = truth(10.0, usize::from(East30K12::POST_FRAMES));
    for (rec, id) in
        [(record::<East30Wide>(&tr, 3), East30Wide::ID), (record::<East30Extreme>(&tr, 3), East30Extreme::ID)]
    {
        assert_eq!(rec[0], EXTENDED_VERSION);
        assert_eq!(rec[1..5], id.to_be_bytes());
        assert_eq!(rec[5..7], 12u16.to_be_bytes());
    }
}

#[cfg(feature = "decode")]
fn round_trip<P: Profile>() {
    let tr = truth(40.0, usize::from(P::POST_FRAMES));
    let rec = record::<P>(&tr, 11);
    let dec = sfk1::Decoder::<P>::new(&rec).unwrap();
    assert_eq!(dec.header().n_live(), 12);
    for ch in dec.channels() {
        assert_eq!(ch.coefs().len(), P::n_coefs());
        let side = usize::from(ch.channel() > 6);
        let worst = ch
            .samples()
            .skip(dec.onset_frame())
            .zip(&tr)
            .map(|(y, t)| (y - 11_000.0 - t[side]).abs())
            .fold(0.0, f64::max);
        assert!(worst < WORST_MAX, "{}: channel {} misses by {worst:.2} counts", P::NAME, ch.channel());
    }
    // another profile's decoder refuses it
    if P::ID != East30K12::ID {
        assert!(matches!(sfk1::Decoder::<East30K12>::new(&rec), Err(sfk1::DecodeError::WrongProfile { .. })));
    }
    #[cfg(feature = "alloc")]
    {
        let d = sfk1::decode(&rec).unwrap();
        assert_eq!(d.profile, P::NAME);
        for (a, b) in d.channels.iter().zip(dec.channels()) {
            assert_eq!((a.coefs.as_slice(), a.bins.as_slice()), (b.coefs(), b.bins()));
        }
    }
}

#[cfg(feature = "decode")]
#[test]
fn every_bundled_profile_round_trips() {
    round_trip::<East30K12>();
    round_trip::<East30K16>();
    round_trip::<East30Wide>();
    round_trip::<East30Extreme>();
    round_trip::<East30Offset>();
}

#[cfg(feature = "alloc")]
#[test]
fn decode_dispatches_to_every_bundled_profile_at_any_channel_count() {
    fn name_of<const N: usize, P: Profile>() -> String {
        let mut mask = vec![0u8; N.div_ceil(8)];
        mask[0] = 0x01;
        let mut enc = Encoder::<N, P>::new(&mask, 2).unwrap();
        for _ in 0..enc.frames_expected() {
            enc.push(25_000, &[11_000; N]).unwrap();
        }
        let mut out = vec![0u8; Encoder::<N, P>::MAX_RECORD_LEN];
        let len = enc.finish(&mut out).unwrap().len;
        let d = sfk1::decode(&out[..len]).unwrap();
        assert_eq!(d.n_channels, u16::try_from(N).unwrap());
        d.profile
    }
    for (got, want) in [
        (name_of::<12, East30K12>(), East30K12::NAME),
        (name_of::<3, East30K12>(), East30K12::NAME),
        (name_of::<12, East30K16>(), East30K16::NAME),
        (name_of::<12, East30Wide>(), East30Wide::NAME),
        (name_of::<5, East30Wide>(), East30Wide::NAME),
        (name_of::<12, East30Extreme>(), East30Extreme::NAME),
        (name_of::<20, East30Extreme>(), East30Extreme::NAME),
        (name_of::<6, East30Offset>(), East30Offset::NAME),
    ] {
        assert_eq!(got, want);
    }
}

/// `P` is accurate from -60 cm/h to `top`.
#[cfg(feature = "decode")]
fn accurate_to<P: Profile>(top: f64) {
    for v in [-60.0, 0.0, 20.0, top] {
        let (_, rmse, worst) = errors::<P>(v);
        assert!(
            rmse <= RMSE_MAX && worst <= WORST_MAX,
            "{} at {v} cm/h: RMS {rmse:.2}, worst {worst:.2} counts",
            P::NAME
        );
    }
}

#[cfg(feature = "decode")]
#[test]
fn each_profile_reproduces_its_flow_range() {
    accurate_to::<East30K12>(70.0);
    accurate_to::<East30K16>(100.0);
    accurate_to::<East30Wide>(150.0);
    accurate_to::<East30Extreme>(200.0);
}

#[cfg(feature = "decode")]
#[test]
fn each_step_up_the_ladder_is_needed() {
    // each profile misses the next one's top flow
    assert!(errors::<East30K12>(100.0).1 > RMSE_MAX);
    assert!(errors::<East30K16>(150.0).1 > RMSE_MAX);
    assert!(errors::<East30Wide>(200.0).1 > RMSE_MAX);
    // and costs more at typical flows
    let len = |l: (usize, f64, f64)| l.0;
    let sizes = [
        len(errors::<East30K12>(10.0)),
        len(errors::<East30K16>(10.0)),
        len(errors::<East30Wide>(10.0)),
        len(errors::<East30Extreme>(10.0)),
    ];
    assert!(sizes.windows(2).all(|w| w[0] < w[1]), "10 cm/h record sizes {sizes:?}");
}
