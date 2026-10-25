//! Golden vectors from the Python reference (`tools/generate.py`).
//!
//! * firmware-mode records: the Rust encoder must produce the same bytes
//!   from the same frames (including the missing ones);
//! * every record: the decoder must recover the same integers and, where
//!   the vector carries the Python decoder's output, the same curve.

use std::path::{Path, PathBuf};

use sfk1::{East30K12, East30K16, Encoder, Profile, MAX_RECORD_LEN};

/// Excitation, then thermistors 1..=12.
type Frame = [i16; 13];

#[cfg_attr(not(feature = "decode"), allow(dead_code))]
struct Record {
    firmware: bool,
    knots: u8,
    missing: u16,
    bytes: Vec<u8>,
    ints: Vec<(Vec<i32>, Vec<i32>)>,
    decoded: Option<(usize, Vec<Vec<f64>>)>,
}

struct Vector {
    name: String,
    live_mask: u16,
    n_bins: u8,
    frames: Vec<Option<Frame>>,
    records: Vec<Record>,
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        a
    }
    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn f64(&mut self) -> f64 {
        f64::from_le_bytes(self.take(8).try_into().unwrap())
    }
    fn varint(&mut self) -> i64 {
        let (mut u, mut shift) = (0u64, 0);
        loop {
            let b = self.u8();
            u |= u64::from(b & 0x7f) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                break;
            }
        }
        (u >> 1) as i64 ^ -((u & 1) as i64)
    }
}

fn parse(path: &Path) -> Vector {
    let data = std::fs::read(path).unwrap();
    let mut c = Cursor(&data);
    assert_eq!(c.take(4), b"SFKV");
    assert_eq!(c.u8(), 1);
    let n = usize::from(c.u8());
    let name = String::from_utf8(c.take(n).to_vec()).unwrap();
    let (live_mask, n_bins, n_frames) = (c.u16(), c.u8(), usize::from(c.u16()));
    let bitmap = c.take(n_frames.div_ceil(8)).to_vec();
    let mut prev = [0i64; 13];
    let frames = (0..n_frames)
        .map(|k| {
            (bitmap[k >> 3] >> (k & 7) & 1 == 1).then(|| {
                let mut f: Frame = [0; 13];
                for (v, p) in f.iter_mut().zip(prev.iter_mut()) {
                    *p += c.varint();
                    *v = i16::try_from(*p).unwrap();
                }
                f
            })
        })
        .collect();
    let n_live = live_mask.count_ones() as usize;
    let records = (0..c.u8())
        .map(|_| {
            let (kind, knots, flags, missing) = (c.u8(), c.u8(), c.u8(), c.u16());
            let len = usize::from(c.u16());
            let bytes = c.take(len).to_vec();
            let ncoef = usize::from(knots) + 4;
            let ints = (0..n_live)
                .map(|_| {
                    let b = (0..n_bins).map(|_| c.i32()).collect();
                    (b, (0..ncoef).map(|_| c.i32()).collect())
                })
                .collect();
            let decoded = (flags & 1 == 1).then(|| {
                let stride = usize::from(c.u16());
                let per = n_frames.div_ceil(stride);
                (stride, (0..n_live).map(|_| (0..per).map(|_| c.f64()).collect()).collect())
            });
            Record { firmware: kind == 0, knots, missing, bytes, ints, decoded }
        })
        .collect();
    assert!(c.0.is_empty(), "{name}: trailing bytes in vector");
    Vector { name, live_mask, n_bins, frames, records }
}

fn vectors() -> Vec<Vector> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sfkv"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 5, "golden vectors missing");
    paths.iter().map(|p| parse(p)).collect()
}

fn encode<P: Profile>(v: &Vector) -> (Vec<u8>, u16) {
    let mut enc = Encoder::<12, P>::new(&v.live_mask.to_be_bytes(), v.n_bins).unwrap();
    assert_eq!(usize::from(enc.frames_expected()), v.frames.len());
    for f in &v.frames {
        match f {
            Some(f) => enc.push(f[0], f[1..].try_into().unwrap()).unwrap(),
            None => enc.push_missing().unwrap(),
        }
    }
    assert!(enc.is_complete());
    let mut out = [0u8; MAX_RECORD_LEN];
    let done = enc.finish(&mut out).unwrap();
    // finish is repeatable and reports the size it needs
    assert_eq!(
        enc.finish(&mut out[..done.len - 1]).unwrap_err(),
        sfk1::EncodeError::BufferTooSmall { needed: done.len }
    );
    (out[..done.len].to_vec(), done.missing)
}

#[test]
fn encoder_is_byte_exact() {
    let mut checked = 0;
    for v in vectors() {
        for r in v.records.iter().filter(|r| r.firmware) {
            let (bytes, missing) = if r.knots == 12 { encode::<East30K12>(&v) } else { encode::<East30K16>(&v) };
            assert_eq!(missing, r.missing, "{} k{}", v.name, r.knots);
            assert_eq!(bytes, r.bytes, "{} k{}: bytes differ", v.name, r.knots);
            println!("{} k{}: {} B, {} missing frames, byte-exact", v.name, r.knots, bytes.len(), missing);
            checked += 1;
        }
    }
    assert_eq!(checked, 10);
}

/// Compare one record with the Python integers and curve; returns
/// (samples compared, worst |diff|).
#[cfg(feature = "decode")]
fn check_decode<P: Profile>(v: &Vector, r: &Record) -> (usize, f64) {
    let dec = sfk1::Decoder::<P>::new(&r.bytes).unwrap();
    let h = dec.header();
    assert_eq!(h.layout(), sfk1::Layout::Compact { version: 0x50 | (r.knots / 4) });
    assert_eq!(h.n_channels(), 12);
    assert_eq!(h.mask(), &v.live_mask.to_be_bytes()[..]);
    assert_eq!(dec.n_frames(), v.frames.len());
    let channels: Vec<_> = dec.channels().collect();
    assert_eq!(channels.len(), r.ints.len());
    assert!(channels.iter().map(sfk1::ChannelCoefs::channel).eq(h.live_channels()));
    let (mut compared, mut worst) = (0, 0.0f64);
    for (i, ch) in channels.iter().enumerate() {
        assert_eq!(ch.bins(), &r.ints[i].0[..], "{}: bins", v.name);
        assert_eq!(ch.coefs(), &r.ints[i].1[..], "{}: coefs", v.name);
        let Some((stride, want)) = &r.decoded else { continue };
        let mut curve = vec![0.0; ch.n_frames()];
        ch.reconstruct(&mut curve).unwrap();
        for (k, &w) in want[i].iter().enumerate() {
            let e = (curve[k * stride] - w).abs();
            assert!(e < 1e-6, "{} ch {} frame {}: {} vs {w}", v.name, ch.channel(), k * stride, curve[k * stride]);
            worst = worst.max(e);
            compared += 1;
        }
    }
    (compared, worst)
}

#[test]
#[cfg(feature = "decode")]
fn decoder_matches_python() {
    let mut worst = 0.0f64;
    let mut compared = 0;
    for v in vectors() {
        for r in &v.records {
            let (n, w) =
                if r.knots == 12 { check_decode::<East30K12>(&v, r) } else { check_decode::<East30K16>(&v, r) };
            compared += n;
            worst = worst.max(w);
            // a record is refused by the other profile
            let wrong = if r.knots == 12 {
                sfk1::Decoder::<East30K16>::new(&r.bytes).map(|_| ())
            } else {
                sfk1::Decoder::<East30K12>::new(&r.bytes).map(|_| ())
            };
            assert!(matches!(wrong, Err(sfk1::DecodeError::WrongProfile { .. })));
        }
    }
    println!("decoder vs Python: {compared} samples, max |diff| {worst:.3e} counts");
    assert!(compared > 10_000);
}

#[test]
#[cfg(feature = "alloc")]
fn alloc_decode_agrees_with_core() {
    fn core<P: Profile>(bytes: &[u8]) -> Vec<(u16, Vec<f64>)> {
        let dec = sfk1::Decoder::<P>::new(bytes).unwrap();
        dec.channels().map(|c| (c.channel(), c.samples().collect())).collect()
    }
    for v in vectors() {
        for r in &v.records {
            let owned = sfk1::decode(&r.bytes).unwrap();
            assert_eq!((owned.n_channels, owned.n_bins, owned.sample_rate_hz), (12, v.n_bins, 25));
            assert_eq!(owned.times().len(), v.frames.len());
            let want = if r.knots == 12 { core::<East30K12>(&r.bytes) } else { core::<East30K16>(&r.bytes) };
            let got: Vec<_> = owned.channels.iter().map(|c| (c.channel, c.samples.clone())).collect();
            assert_eq!(got, want);
        }
    }
}

#[test]
#[cfg(feature = "decode")]
fn every_truncation_is_rejected() {
    fn check<P: Profile>(name: &str, bytes: &[u8]) {
        sfk1::Decoder::<P>::new(bytes).unwrap();
        for n in 0..bytes.len() {
            assert!(sfk1::Decoder::<P>::new(&bytes[..n]).is_err(), "{name} truncated to {n}");
        }
        let mut long = bytes.to_vec();
        long.push(0);
        assert_eq!(sfk1::Decoder::<P>::new(&long).unwrap_err(), sfk1::DecodeError::TrailingBytes);
    }
    for v in vectors() {
        for r in &v.records {
            if r.knots == 12 {
                check::<East30K12>(&v.name, &r.bytes);
            } else {
                check::<East30K16>(&v.name, &r.bytes);
            }
        }
    }
}
