//! Profiles trained at a probe's own thermistor positions:
//!
//! * `East30At1cm` (`tools/generate.py --profile east30_1cm --struct
//!   East30At1cm --positions='1,0 -1,0'`), a test profile for 1.0 cm
//!   spacing;
//! * the bundled [`East30Offset`]: 10 mm above the heater, 5 mm below.
//!
//! Each is checked against East30's 0.6 cm tables on simulated pulses at
//! its own positions; tables never change what decodes, only record size.

use sfk1::{Decoder, East30K12, East30Offset, Encoder, Profile};

mod east30_1cm {
    include!("profiles/east30_1cm.rs");
}
use east30_1cm::East30At1cm;

/// Instantaneous moving line source at `x` cm along the flow (k in cm²/s,
/// v in cm/h), counts above baseline. The amplitude follows the physics: it
/// is `amp` for a 1 cm thermistor at its no-flow peak.
fn pulse(t: f64, x: f64, k: f64, v_cm_h: f64, amp: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    let v = v_cm_h / 3600.0;
    let peak_1cm = (-1.0f64).exp() * 4.0 * k; // max over t of exp(-1/(4kt))/t
    amp / peak_1cm * (-(x - v * t).powi(2) / (4.0 * k * t)).exp() / t
}

/// Thermistor `c` of a probe with `N` thermistors, the first half at
/// `pos[0]` cm and the rest at `pos[1]`.
fn record<const N: usize, P: Profile>(pos: [f64; 2], k: f64, v: f64, seed: u32) -> Vec<u8> {
    let mask = (1u16 << N) - 1;
    let mask = if N > 8 { mask.to_be_bytes().to_vec() } else { vec![u8::try_from(mask).unwrap()] };
    let mut enc = Encoder::<N, P>::new(&mask, 12).unwrap();
    let mut s = seed | 1;
    let mut noise = || {
        // sum of four uniforms, sigma 1.5 counts
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
    for n in 0..enc.frames_expected() {
        let t = (f64::from(n) - f64::from(enc.onset_frame())) / 25.0;
        let therm: [i16; N] = core::array::from_fn(|c| {
            let x = pos[usize::from(c >= N / 2)];
            let amp = 200.0 + 10.0 * c as f64;
            (11_000.0 + pulse(t, x, k, v, amp) + noise()).round() as i16
        });
        enc.push(25_000, &therm).unwrap();
    }
    let mut out = vec![0u8; 4096];
    let len = enc.finish(&mut out).unwrap().len;
    out.truncate(len);
    out
}

/// Mean record length over a small grid of diffusivities and velocities.
fn mean_len<const N: usize, P: Profile>(pos: [f64; 2]) -> usize {
    let mut total = 0;
    for (i, k) in [0.002, 0.0025, 0.003].into_iter().enumerate() {
        for (j, v) in [0.0, 10.0, 25.0].into_iter().enumerate() {
            let seed = 0x9e37_79b9 ^ (u32::try_from(3 * i + j).unwrap() << 8);
            total += record::<N, P>(pos, k, v, seed).len();
        }
    }
    total / 9
}

#[test]
fn generated_profiles_are_valid_and_distinct() {
    assert_eq!(East30At1cm::validate(), Ok(()));
    assert_eq!(East30Offset::validate(), Ok(()));
    assert_ne!(East30At1cm::ID, East30K12::ID);
    assert_ne!(East30Offset::ID, East30K12::ID);
    for (period, post, t0) in [
        (East30At1cm::FRAME_PERIOD_US, East30At1cm::POST_FRAMES, East30At1cm::T0_FRAMES),
        (East30Offset::FRAME_PERIOD_US, East30Offset::POST_FRAMES, East30Offset::T0_FRAMES),
    ] {
        assert_eq!((period, post, t0), (East30K12::FRAME_PERIOD_US, East30K12::POST_FRAMES, East30K12::T0_FRAMES));
    }
}

#[test]
fn tables_trained_at_1cm_make_smaller_1cm_records() {
    let (east30, at1cm) = (mean_len::<12, East30K12>([1.0, -1.0]), mean_len::<12, East30At1cm>([1.0, -1.0]));
    assert!(at1cm + 40 < east30, "mean record: East30 tables {east30} B, 1 cm tables {at1cm} B");
}

#[test]
fn the_offset_profile_makes_smaller_offset_probe_records() {
    let pos = [1.0, -0.5]; // 10 mm above (downstream), 5 mm below
    let (east30, offset) = (mean_len::<6, East30K12>(pos), mean_len::<6, East30Offset>(pos));
    assert!(offset + 15 < east30, "mean 6-thermistor record: East30 tables {east30} B, offset tables {offset} B");
}

#[test]
fn an_upside_down_offset_probe_still_decodes_exactly() {
    // installed flipped: 5 mm above, 10 mm below. Bigger records, same curve.
    let pos = [0.5, -1.0];
    let (east30, offset) =
        (record::<6, East30K12>(pos, 0.0025, 10.0, 7), record::<6, East30Offset>(pos, 0.0025, 10.0, 7));
    let a = Decoder::<East30K12>::new(&east30).unwrap();
    let b = Decoder::<East30Offset>::new(&offset).unwrap();
    for (ca, cb) in a.channels().zip(b.channels()) {
        assert_eq!(ca.bins(), cb.bins());
        let worst = ca.samples().zip(cb.samples()).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
        assert!(worst < 2.0, "channel {}: decodes differ by {worst} counts", ca.channel());
    }
    #[cfg(feature = "alloc")]
    assert_eq!(sfk1::decode(&offset).unwrap().profile, East30Offset::NAME);
}
