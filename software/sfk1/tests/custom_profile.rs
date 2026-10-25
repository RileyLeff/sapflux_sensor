//! A profile defined outside the crate: a 48-thermistor probe at 10 Hz with
//! tables from `tools/generate.py --profile probe48_10hz`.

use sfk1::{East30K12, Encoder, Profile};

mod probe48 {
    include!("profiles/probe48_10hz.rs");
}

struct Probe48;

impl Profile for Probe48 {
    const NAME: &'static str = "probe48 10 Hz";
    const FRAME_PERIOD_US: u32 = 100_000;
    const BIN_FRAMES: u16 = 50;
    type Bins = [i32; 12];
    const POST_FRAMES: u16 = 1811;
    const T0_FRAMES: u32 = 20;
    const KNOTS: u8 = 12;
    type Acc = [i64; 16];
    const KF_Q28: i64 = probe48::KF_Q28;
    const MQ: &'static [i32] = probe48::MQ.as_flattened();
    const D: &'static [f64] = probe48::D.as_flattened();
    const K_TABLE: &'static [u8] = &probe48::K_TABLE;
}

/// A heat-pulse-like rise in counts at `t` s after onset, different per
/// channel.
fn rise(c: usize, t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    let amp = 150.0 + 10.0 * c as f64;
    let tau = 20.0 + c as f64;
    amp * (1.0 - (-t / tau).exp()) * (-t / 400.0).exp()
}

#[test]
fn probe48_is_a_valid_distinct_profile() {
    assert_eq!(Probe48::validate(), Ok(()));
    assert_ne!(Probe48::ID, East30K12::ID);
    assert_eq!(Probe48::COMPACT_VERSION, None);
    assert_eq!(Encoder::<48, Probe48>::HEADER_LEN, 16);
}

#[test]
fn probe48_round_trip() {
    let mut enc = Encoder::<48, Probe48>::new(&[0xff; 6], 12).unwrap();
    let onset = enc.onset_frame();
    assert_eq!((onset, enc.frames_expected()), (600, 600 + 1811));
    let mut truth = vec![[0.0f64; 48]; usize::from(enc.frames_expected())];
    let mut noise = 0x2545_f491u32;
    for n in 0..enc.frames_expected() {
        let t = (f64::from(n) - f64::from(onset)) / 10.0;
        let mut therm = [0i16; 48];
        for (c, v) in therm.iter_mut().enumerate() {
            let x = 9_000.0 + 40.0 * c as f64 + 0.01 * t + rise(c, t);
            truth[usize::from(n)][c] = x;
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            *v = (x + f64::from(noise % 5) - 2.0).round() as i16;
        }
        if n % 97 == 5 {
            enc.push_missing().unwrap();
        } else {
            enc.push(20_000, &therm).unwrap();
        }
    }
    let mut out = vec![0u8; Encoder::<48, Probe48>::MAX_RECORD_LEN];
    let done = enc.finish(&mut out).unwrap();
    let rec = &out[..done.len];
    println!(
        "probe48: 48 channels, {} B ({:.1} B/channel), {} missing",
        done.len,
        done.len as f64 / 48.0,
        done.missing
    );

    #[cfg(feature = "decode")]
    {
        use sfk1::{DecodeError, Decoder, Layout};
        let dec = Decoder::<Probe48>::new(rec).unwrap();
        assert_eq!(dec.header().layout(), Layout::Extended { profile_id: Probe48::ID });
        assert_eq!((dec.header().n_channels(), dec.header().n_live()), (48, 48));
        assert!((dec.time_s(dec.onset_frame() + 10) - 1.0).abs() < 1e-12);
        let mut worst = 0.0f64;
        for ch in dec.channels() {
            let c = usize::from(ch.channel()) - 1;
            let (mut se, mut n) = (0.0, 0.0);
            for (i, v) in ch.samples().enumerate().skip(usize::from(onset)) {
                se += (v - truth[i][c]).powi(2);
                n += 1.0;
            }
            worst = worst.max((se / n).sqrt());
        }
        println!("probe48: worst post-onset RMS error {worst:.2} counts");
        assert!(worst < 3.0, "{worst}");
        assert!(matches!(Decoder::<East30K12>::new(rec), Err(DecodeError::WrongProfile { .. })));
        #[cfg(feature = "alloc")]
        assert!(matches!(sfk1::decode(rec), Err(DecodeError::WrongProfile { .. })));
    }
    #[cfg(not(feature = "decode"))]
    let _ = (rec, truth);
}

/// Probe48's tables on a 0.4 Hz grid (2.5 s frames), as a large multiplexed
/// array might run. The spline basis lives in frames, so only the time axis
/// and the id change.
struct Probe48Slow;

impl Profile for Probe48Slow {
    const NAME: &'static str = "probe48 0.4 Hz";
    const FRAME_PERIOD_US: u32 = 2_500_000;
    const BIN_FRAMES: u16 = Probe48::BIN_FRAMES;
    type Bins = [i32; 12];
    const POST_FRAMES: u16 = Probe48::POST_FRAMES;
    const T0_FRAMES: u32 = Probe48::T0_FRAMES;
    const KNOTS: u8 = Probe48::KNOTS;
    type Acc = [i64; 16];
    const KF_Q28: i64 = Probe48::KF_Q28;
    const MQ: &'static [i32] = Probe48::MQ;
    const D: &'static [f64] = Probe48::D;
    const K_TABLE: &'static [u8] = Probe48::K_TABLE;
}

/// A record of the same 48 frame-indexed rises, on profile `P`'s grid.
fn record<P: Profile>() -> Vec<u8> {
    let mut enc = Encoder::<48, P>::new(&[0xff; 6], 12).unwrap();
    for n in 0..enc.frames_expected() {
        let k = f64::from(n) - f64::from(enc.onset_frame());
        let therm: [i16; 48] = core::array::from_fn(|c| (9_000.0 + rise(c, k)) as i16);
        enc.push(20_000, &therm).unwrap();
    }
    let mut out = vec![0u8; 4096];
    let len = enc.finish(&mut out).unwrap().len;
    out.truncate(len);
    out
}

#[test]
fn a_sub_hertz_grid_changes_only_the_time_axis() {
    assert_eq!(Probe48Slow::validate(), Ok(()));
    assert_ne!(Probe48Slow::ID, Probe48::ID);
    let (fast, slow) = (record::<Probe48>(), record::<Probe48Slow>());
    assert_eq!(fast.len(), slow.len());
    #[cfg(feature = "decode")]
    {
        let fast = sfk1::Decoder::<Probe48>::new(&fast).unwrap();
        let slow = sfk1::Decoder::<Probe48Slow>::new(&slow).unwrap();
        let onset = slow.onset_frame();
        assert_eq!((slow.time_s(onset + 1), slow.time_s(onset - 2), fast.time_s(onset + 1)), (2.5, -5.0, 0.1));
        for (f, s) in fast.channels().zip(slow.channels()) {
            assert_eq!((f.coefs(), f.bins()), (s.coefs(), s.bins()));
        }
    }
}
