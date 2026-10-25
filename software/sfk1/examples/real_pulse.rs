//! Encode a real bench capture with the board encoder and decode it again.
//!
//! ```text
//! cargo run --example real_pulse --features std -- \
//!     [SESSION_DIR] [RUN_ID] [LIVE_MASK] [--knots 16] [--out record.bin]
//! ```
//!
//! Defaults to the 24 Aug soaked-stem pulse (thermistors 2..=6). The bench
//! logger samples at about 24 Hz with two conversions per frame, so frames
//! are decimated by two around onset and placed on the 25 Hz grid the board
//! uses by `round(25 t)`; grid slots no frame lands on are pushed as missing
//! (the same placement as `fw_frames` in `encoder_spec.py`, so
//! `tools/generate.py --check-record` can check the record from Python).

use std::error::Error;
use std::path::PathBuf;

use sfk1::{East30K12, East30K16, Encoder, Profile, MAX_RECORD_LEN};

/// Excitation, then thermistors 1..=12.
type Frame = [i16; 13];

const DEFAULT_SESSION: &str = "../sap-flux-monitor/captures/session_20260824_203902";
const DEFAULT_RUN: i64 = 1_787_618_451;

struct Row {
    t_us: i64,
    run_id: i64,
    heater: bool,
    raw: [i16; 7],
}

fn read_rows(path: &std::path::Path) -> Result<Vec<Row>, Box<dyn Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().ok_or("empty csv")?.split(',').collect();
    let col = |name: &str| head.iter().position(|h| *h == name).ok_or(format!("no column {name}"));
    let (c_t, c_run, c_heat) = (col("monotonic_us")?, col("run_id")?, col("heater_active")?);
    let mut c_raw = [col("excitation_raw")?; 7];
    for (k, c) in c_raw.iter_mut().enumerate().skip(1) {
        *c = col(&format!("t{k}_raw"))?;
    }
    let mut rows = Vec::new();
    for line in lines {
        let f: Vec<&str> = line.split(',').collect();
        let mut raw = [0i16; 7];
        for (r, &c) in raw.iter_mut().zip(&c_raw) {
            // an empty cell (dead channel) becomes 0, as in the reference
            *r = f[c].parse::<f64>().map_or(0, |v| v.round() as i16);
        }
        rows.push(Row { t_us: f[c_t].parse()?, run_id: f[c_run].parse()?, heater: f[c_heat] == "1", raw });
    }
    Ok(rows)
}

/// The window on the 25 Hz grid: frames, which are present, baseline bins.
type Grid = (Vec<Frame>, Vec<bool>, u8);

fn grid(rows: &[Row], run_id: i64) -> Result<Grid, Box<dyn Error>> {
    let on = rows.iter().position(|r| r.run_id == run_id && r.heater).ok_or("run has no heater onset")?;
    // the contiguous block around onset (no gap over 200 ms)
    let ok = |i: usize| rows[i].t_us > rows[i - 1].t_us && rows[i].t_us - rows[i - 1].t_us <= 200_000;
    let a = (1..=on).rev().find(|&i| !ok(i)).unwrap_or(0);
    let b = (on + 1..rows.len()).find(|&i| !ok(i)).unwrap_or(rows.len());
    let t = |i: usize| (rows[i].t_us - rows[on].t_us) as f64 / 1e6;
    let picked: Vec<usize> =
        (a..b).filter(|i| (i - a) % 2 == (on - a) % 2).filter(|&i| (-60.0..=181.0).contains(&t(i))).collect();
    let slot = |i: usize| (t(i) * 25.0).round_ties_even() as i64;
    let first = picked.iter().map(|&i| slot(i)).min().ok_or("no frames")?;
    let n_bins = ((-first + 124) / 125).clamp(2, 12) as u8; // ceil(-first / 125)
    let npre = i64::from(n_bins) * 125;
    let n = (npre + 4526) as usize;
    let mut frames = vec![[0i16; 13]; n];
    let mut present = vec![false; n];
    for &i in &picked {
        let k = slot(i) + npre;
        if (0..n as i64).contains(&k) && !present[k as usize] {
            frames[k as usize][..7].copy_from_slice(&rows[i].raw);
            present[k as usize] = true;
        }
    }
    Ok((frames, present, n_bins))
}

fn encode<P: Profile>(mask: u16, n_bins: u8, frames: &[Frame], present: &[bool]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut enc = Encoder::<12, P>::new(&mask.to_be_bytes(), n_bins)?;
    for (f, &p) in frames.iter().zip(present) {
        if p {
            let therm: &[i16; 12] = f[1..].try_into()?;
            enc.push(f[0], therm)?;
        } else {
            enc.push_missing()?;
        }
    }
    println!("encoder state: {} B on the stack", std::mem::size_of::<Encoder<12, P>>());
    let mut out = [0u8; MAX_RECORD_LEN];
    let done = enc.finish(&mut out)?;
    println!("record: {} B, {} of {} frames missing, E_ref {}", done.len, done.missing, frames.len(), done.e_ref);
    Ok(out[..done.len].to_vec())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut pos = Vec::new();
    let (mut knots, mut out) = (12, None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--knots" => knots = args.next().ok_or("--knots N")?.parse()?,
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out FILE")?)),
            _ => pos.push(a),
        }
    }
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let session = pos.first().map_or_else(|| crate_dir.join(DEFAULT_SESSION), PathBuf::from);
    let run_id = pos.get(1).map_or(Ok(DEFAULT_RUN), |s| s.parse())?;
    let mask = pos.get(2).map_or(Ok(0b11_1110), |s| u16::from_str_radix(s.trim_start_matches("0b"), 2))?;

    let rows = read_rows(&session.join("samples.csv"))?;
    let (frames, present, n_bins) = grid(&rows, run_id)?;
    println!("{}: run {run_id}, {} grid frames, {n_bins} baseline bins", session.display(), frames.len());
    let raw_bytes = present.iter().filter(|&&p| p).count() * 13 * 2;
    let record = match knots {
        12 => encode::<East30K12>(mask, n_bins, &frames, &present)?,
        16 => encode::<East30K16>(mask, n_bins, &frames, &present)?,
        _ => return Err("--knots must be 12 or 16".into()),
    };
    #[allow(clippy::cast_precision_loss)]
    let ratio = raw_bytes as f64 / record.len() as f64;
    println!("raw {raw_bytes} B -> {} B ({ratio:.0}x)", record.len());
    if let Some(path) = out {
        std::fs::write(&path, &record)?;
        println!("wrote {}", path.display());
    }

    // decode and compare with the frame's own ratiometric counts
    let dec = sfk1::decode(&record)?;
    let e_ref = f64::from(dec.e_ref);
    println!("ch   max|err|  rms err  (counts at E_ref, present frames; includes the sensor noise)");
    for ch in &dec.channels {
        let (mut worst, mut ss, mut n) = (0.0f64, 0.0, 0usize);
        for (k, f) in frames.iter().enumerate() {
            if !present[k] || f[0] <= 0 {
                continue;
            }
            let y = f64::from(f[usize::from(ch.channel)]) * e_ref / f64::from(f[0]);
            let e = ch.samples[k] - y;
            worst = worst.max(e.abs());
            ss += e * e;
            n += 1;
        }
        #[allow(clippy::cast_precision_loss)]
        let rms = (ss / n.max(1) as f64).sqrt();
        println!("t{:<2} {worst:9.2} {rms:8.2}", ch.channel);
    }
    Ok(())
}
