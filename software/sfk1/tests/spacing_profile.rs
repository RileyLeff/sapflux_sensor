//! A generated profile for East30-style probes at 1.0 cm spacing
//! (`tools/generate.py --profile east30_1cm --struct East30At1cm
//! --positions='1,0 -1,0'`): same grid as East30, tables trained at 1 cm.

use sfk1::{East30K12, Encoder, Profile};

mod east30_1cm {
    include!("profiles/east30_1cm.rs");
}
use east30_1cm::East30At1cm;

/// Instantaneous moving line source at `x` cm along the flow, counts above
/// baseline; `amp` scales a 1 cm downstream, no-flow peak.
fn pulse(t: f64, x: f64, k: f64, v_cm_h: f64, amp: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    let v = v_cm_h / 3600.0;
    let peak = (-1.0f64).exp() / (1.0 / (4.0 * k)); // v = 0 peak of exp(-1/(4kt))/t at x = 1
    amp / peak * (-(x - v * t).powi(2) / (4.0 * k * t)).exp() / t
}

/// Record length for six thermistors 1 cm downstream and six 1 cm upstream.
fn record_len<P: Profile>(k: f64, v: f64, seed: u32) -> usize {
    let mut enc = Encoder::<12, P>::new(&[0x0f, 0xff], 12).unwrap();
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
        let therm: [i16; 12] = core::array::from_fn(|c| {
            let x = if c < 6 { 1.0 } else { -1.0 };
            let amp = 200.0 + 10.0 * c as f64;
            (11_000.0 + pulse(t, x, k, v, amp) + noise()).round() as i16
        });
        enc.push(25_000, &therm).unwrap();
    }
    let mut out = [0u8; 4096];
    enc.finish(&mut out).unwrap().len
}

#[test]
fn the_generated_profile_is_valid_and_distinct() {
    assert_eq!(East30At1cm::validate(), Ok(()));
    assert_ne!(East30At1cm::ID, East30K12::ID);
    assert_eq!(
        (East30At1cm::FRAME_PERIOD_US, East30At1cm::POST_FRAMES, East30At1cm::T0_FRAMES),
        (East30K12::FRAME_PERIOD_US, East30K12::POST_FRAMES, East30K12::T0_FRAMES)
    );
}

#[test]
fn tables_trained_at_1cm_make_smaller_1cm_records() {
    let (mut east30, mut at1cm, mut runs) = (0, 0, 0);
    for (i, k) in [0.002, 0.0025, 0.003].into_iter().enumerate() {
        for (j, v) in [0.0, 10.0, 25.0].into_iter().enumerate() {
            let seed = 0x9e37_79b9 ^ (u32::try_from(3 * i + j).unwrap() << 8);
            east30 += record_len::<East30K12>(k, v, seed);
            at1cm += record_len::<East30At1cm>(k, v, seed);
            runs += 1;
        }
    }
    let (east30, at1cm) = (east30 / runs, at1cm / runs);
    assert!(at1cm + 40 < east30, "mean record: East30 tables {east30} B, 1 cm tables {at1cm} B");
}
