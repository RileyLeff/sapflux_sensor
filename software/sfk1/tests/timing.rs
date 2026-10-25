//! Continuous-time evaluation and per-channel sample offsets (mux skew).
#![cfg(feature = "decode")]

use sfk1::{Decoder, East30K12, Encoder12, MAX_RECORD_LEN};

/// A line-source heat pulse at 0.6 cm (k = 0.0025 cm²/s), counts.
fn pulse(t: f64) -> f64 {
    if t <= 0.0 {
        return 11_000.0;
    }
    let tau = 0.36 / 0.0025;
    11_000.0 + 300.0 * (tau / t) * (1.0 - tau / t).exp()
}

/// One live channel sampled `delta` seconds after each 25 Hz frame instant.
fn record(delta: f64) -> Vec<u8> {
    let mut enc = Encoder12::new(&[0x00, 0x01], 12).unwrap();
    for n in 0..enc.frames_expected() {
        let t = (f64::from(n) - f64::from(enc.onset_frame())) / 25.0 + delta;
        let mut therm = [0i16; 12];
        therm[0] = pulse(t).round() as i16;
        enc.push(25_000, &therm).unwrap();
    }
    let mut out = [0u8; MAX_RECORD_LEN];
    let len = enc.finish(&mut out).unwrap().len;
    out[..len].to_vec()
}

#[test]
fn the_curve_matches_the_samples_at_frame_instants() {
    let rec = record(0.0);
    let dec = Decoder::<East30K12>::new(&rec).unwrap();
    let ch = dec.channels().next().unwrap();
    let curve = ch.curve();
    for (i, v) in ch.samples().enumerate() {
        assert_eq!(curve.at(dec.time_s(i)), v, "frame {i}");
    }
}

#[test]
fn shifting_by_the_sample_offset_recovers_the_true_timing() {
    let delta = 0.5;
    let rec = record(delta);
    let dec = Decoder::<East30K12>::new(&rec).unwrap();
    let ch = dec.channels().next().unwrap();
    let curve = ch.curve();
    let err = |shift: f64| {
        (0..1800).map(|k| 1.0 + 0.1 * f64::from(k)).map(|t| (curve.at(t - shift) - pulse(t)).abs()).fold(0.0, f64::max)
    };
    let (shifted, naive) = (err(delta), err(0.0));
    assert!(shifted < 0.5, "shifted max error {shifted} counts");
    assert!(naive > 5.0 * shifted, "naive {naive} vs shifted {shifted}");
}
