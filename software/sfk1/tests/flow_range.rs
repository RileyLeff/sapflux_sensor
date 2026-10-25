//! Every bundled preset in [`sfk1::BUNDLED`]: valid, distinct, round trips
//! through [`sfk1::decode`], and reproduces its stated flow range on
//! simulated pulses, which the next rung up the knot ladder is needed to
//! extend.
#![cfg_attr(not(feature = "decode"), allow(dead_code, unused_imports))]

use sfk1::{Encoder, PresetInfo, Profile, ProfileError, BUNDLED, EXTENDED_VERSION};

/// Diffusivity, cm²/s.
const K: f64 = 0.0025;
/// Thermistors per simulated record: one probe, three at each position.
const N: usize = 6;
/// Accurate: full-curve RMS error vs truth and worst single-sample miss,
/// counts, from onset to the end of the window.
const RMSE_MAX: f64 = 0.5;
const WORST_MAX: f64 = 5.0;

/// A 1 s heat pulse at `x` cm, `v` cm/h: the moving instantaneous line
/// source averaged over the pulse, unscaled.
fn shape(t: f64, x: f64, v: f64) -> f64 {
    let inst = |t: f64| {
        if t <= 0.0 {
            return 0.0;
        }
        (-(x - v / 3600.0 * t).powi(2) / (4.0 * K * t)).exp() / t
    };
    (0..20).map(|i| inst(t - (f64::from(i) + 0.5) / 20.0)).sum::<f64>() / 20.0
}

/// The rise above baseline for each frame from onset at the two positions,
/// scaled to 350 counts at the no-flow 0.6 cm peak.
fn truth(x: [f64; 2], v: f64, n_post: usize) -> Vec<[f64; 2]> {
    let peak = (0..4000).map(|i| shape(f64::from(i) * 0.05, 0.6, 0.0)).fold(0.0, f64::max);
    #[allow(clippy::cast_precision_loss)]
    (0..n_post).map(|i| x.map(|x| 350.0 * shape(i as f64 / 25.0, x, v) / peak)).collect()
}

/// Encode one probe's pulse with 1.5 counts RMS of noise.
fn record<P: Profile>(truth: &[[f64; 2]], seed: u32) -> Vec<u8> {
    let mut enc = Encoder::<N, P>::new(&[0x3f], 12).unwrap();
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
        let therm: [i16; N] =
            core::array::from_fn(|c| (11_000.0 + tr[usize::from(c >= N / 2)] + noise()).round() as i16);
        enc.push(25_000, &therm).unwrap();
    }
    let mut out = vec![0u8; Encoder::<N, P>::MAX_RECORD_LEN];
    let len = enc.finish(&mut out).unwrap().len;
    out.truncate(len);
    out
}

/// Record length, full-curve RMS error and worst miss vs truth, counts.
#[cfg(feature = "decode")]
fn errors<P: Profile>(x: [f64; 2], v: f64) -> (usize, f64, f64) {
    let tr = truth(x, v, usize::from(P::POST_FRAMES));
    let rec = record::<P>(&tr, 0x9e37_79b9);
    let dec = sfk1::Decoder::<P>::new(&rec).unwrap();
    let on = dec.onset_frame();
    let (mut se, mut n, mut worst) = (0.0, 0.0, 0.0f64);
    for ch in dec.channels() {
        let side = usize::from(usize::from(ch.channel()) > N / 2);
        for (i, y) in ch.samples().skip(on).enumerate() {
            let e = y - 11_000.0 - tr[i][side];
            se += e * e;
            n += 1.0;
            worst = worst.max(e.abs());
        }
    }
    (rec.len(), (se / n).sqrt(), worst)
}

/// Encode a 12- and a 6-thermistor pulse and decode them with the
/// profile's decoder and with `sfk1::decode`.
#[cfg(feature = "decode")]
fn round_trip<P: Profile>(x: [f64; 2]) {
    let tr = truth(x, 40.0, usize::from(P::POST_FRAMES));
    let rec = record::<P>(&tr, 11);
    assert_eq!(rec[0], EXTENDED_VERSION);
    assert_eq!(rec[1..5], P::ID.to_be_bytes());
    let dec = sfk1::Decoder::<P>::new(&rec).unwrap();
    assert_eq!(dec.header().n_live(), 6);
    for ch in dec.channels() {
        assert_eq!(ch.coefs().len(), P::n_coefs());
        let side = usize::from(usize::from(ch.channel()) > N / 2);
        let worst = ch
            .samples()
            .skip(dec.onset_frame())
            .zip(&tr)
            .map(|(y, t)| (y - 11_000.0 - t[side]).abs())
            .fold(0.0, f64::max);
        assert!(worst < WORST_MAX, "{}: channel {} misses by {worst:.2} counts", P::NAME, ch.channel());
    }
    #[cfg(feature = "alloc")]
    {
        let d = sfk1::decode(&rec).unwrap();
        assert_eq!(d.profile, P::NAME);
        for (a, b) in d.channels.iter().zip(dec.channels()) {
            assert_eq!((a.coefs.as_slice(), a.bins.as_slice()), (b.coefs(), b.bins()));
        }
        // 12 channels: the compact header where the profile has one
        let mut enc = Encoder::<12, P>::new(&[0x0f, 0xff], 2).unwrap();
        for _ in 0..enc.frames_expected() {
            enc.push(25_000, &[11_000; 12]).unwrap();
        }
        let mut out = vec![0u8; Encoder::<12, P>::MAX_RECORD_LEN];
        let len = enc.finish(&mut out).unwrap().len;
        assert_eq!(out[0], P::COMPACT_VERSION.unwrap_or(EXTENDED_VERSION));
        let d = sfk1::decode(&out[..len]).unwrap();
        assert_eq!((d.profile.as_str(), d.n_channels, d.channels.len()), (P::NAME, 12, 12));
    }
}

/// One bundled type, as generic functions.
struct Preset {
    type_name: &'static str,
    id: u32,
    knots: u8,
    compact: Option<u8>,
    validate: fn() -> Result<(), ProfileError>,
    #[cfg(feature = "decode")]
    round_trip: fn([f64; 2]),
    #[cfg(feature = "decode")]
    errors: fn([f64; 2], f64) -> (usize, f64, f64),
}

macro_rules! presets {
    ($($ty:ident: $g:ident, $p:ident, ($a:literal, $b:literal), ($c:literal, $d:literal), $e:literal, $f:literal;)*) => {
        vec![$(Preset {
            type_name: stringify!($ty),
            id: <sfk1::$ty as Profile>::ID,
            knots: <sfk1::$ty as Profile>::KNOTS,
            compact: <sfk1::$ty as Profile>::COMPACT_VERSION,
            validate: <sfk1::$ty as Profile>::validate,
            #[cfg(feature = "decode")]
            round_trip: round_trip::<sfk1::$ty>,
            #[cfg(feature = "decode")]
            errors: errors::<sfk1::$ty>,
        }),*]
    };
}

fn presets() -> Vec<(Preset, &'static PresetInfo)> {
    let types = sfk1::__bundled!(presets);
    assert_eq!(types.len(), BUNDLED.len());
    types.into_iter().zip(BUNDLED).collect()
}

#[test]
fn the_catalog_lists_every_type_once() {
    let all = presets();
    assert_eq!(all.len(), 24);
    for (i, (p, info)) in all.iter().enumerate() {
        assert_eq!(
            (p.type_name, p.id, p.knots, p.compact),
            (info.type_name, info.id, info.knots, info.compact_version)
        );
        assert_eq!((p.validate)(), Ok(()), "{}", p.type_name);
        assert!(all[i + 1..].iter().all(|(q, _)| q.id != p.id), "{} shares id {:#010x}", p.type_name, p.id);
        assert_eq!(PresetInfo::by_id(p.id).map(|i| i.type_name), Some(p.type_name));
        assert_eq!(info.t0_s, 2.0);
        assert!(info.trained_cm_h.1 <= info.accurate_cm_h.1);
    }
    let compact: Vec<_> = BUNDLED.iter().filter_map(|i| i.compact_version.map(|v| (v, i.type_name))).collect();
    assert_eq!(compact, [(0x53, "East30K12"), (0x54, "East30K16")]);
    assert_eq!(PresetInfo::by_compact_version(0x54).map(|i| i.type_name), Some("East30K16"));
    assert_eq!(PresetInfo::by_id(0), None);
}

#[cfg(feature = "decode")]
#[test]
fn every_preset_round_trips_and_decode_dispatches_to_it() {
    for (p, info) in presets() {
        (p.round_trip)(info.geometry.positions_cm());
    }
}

/// The rung above `info`: same geometry and prior, four more knots.
fn next_rung(info: &PresetInfo) -> Option<&'static PresetInfo> {
    BUNDLED.iter().find(|n| n.geometry == info.geometry && n.prior == info.prior && n.knots == info.knots + 4)
}

#[cfg(feature = "decode")]
fn check_ranges(geometry: sfk1::Geometry) {
    for (p, info) in presets().into_iter().filter(|(_, i)| i.geometry == geometry) {
        let x = geometry.positions_cm();
        let (lo, hi) = info.accurate_cm_h;
        for v in [lo, 10, hi] {
            let (_, rmse, worst) = (p.errors)(x, f64::from(v));
            assert!(
                rmse <= RMSE_MAX && worst <= WORST_MAX,
                "{} at {v} cm/h: RMS {rmse:.2}, worst {worst:.2}",
                p.type_name
            );
        }
        if let Some(next) = next_rung(info) {
            let v = f64::from(next.accurate_cm_h.1);
            let (_, rmse, worst) = (p.errors)(x, v);
            assert!(
                rmse > RMSE_MAX || worst > WORST_MAX,
                "{} is already accurate at {v} cm/h ({}'s top)",
                p.type_name,
                next.type_name
            );
        }
        // the catalog's typical size, from one seed instead of three
        let typical = [0, 5, 10, 15, 20].map(|v| (p.errors)(x, f64::from(v)).0).iter().sum::<usize>() / 5;
        assert!(
            typical.abs_diff(usize::from(info.typical_bytes)) <= 4,
            "{}: {typical} B, catalog {}",
            p.type_name,
            info.typical_bytes
        );
    }
}

#[cfg(feature = "decode")]
#[test]
fn east30_presets_reproduce_their_flow_range() {
    check_ranges(sfk1::Geometry::East30);
}

#[cfg(feature = "decode")]
#[test]
fn offset_presets_reproduce_their_flow_range() {
    check_ranges(sfk1::Geometry::East30Offset);
}

#[test]
fn each_rung_costs_more_at_typical_flows() {
    for info in BUNDLED {
        if let Some(next) = next_rung(info) {
            assert!(next.typical_bytes > info.typical_bytes, "{} vs {}", info.type_name, next.type_name);
        }
    }
}
