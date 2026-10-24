//! Pulse windows: verification, decoding back to raw counts, CSV output,
//! and a reference binner that reduces raw frames the way the board does
//! (used by the gateway simulator and tests).

use crate::board::{self, BoardMsg, WindowInfo, WINDOW_FLAG_TIME_INVALID, WINDOW_INFO_FRAME_LEN};
use sap_flux_protocol::field::{decode_bin_value_x16, encode_bin_value, mean_raw_rounded};
use sapflux_uplink::crc32;
use serde::Serialize;
use std::io::Write;

pub use sap_flux_protocol::{BIN_VALUE_MISSING as SENTINEL, WINDOW_CHUNK_DATA_LEN as CHUNK_BYTES};
/// Data values are 16× the delta from `base_raw`.
pub const SCALE: f64 = sap_flux_protocol::BIN_VALUE_SCALE as f64;

/// Channel names for the standard 13-channel layout.
pub fn channel_names(channels: u8) -> Vec<String> {
    if channels == 13 {
        let mut v: Vec<String> = (1..=6).map(|i| format!("p1_t{i}")).collect();
        v.extend((1..=6).map(|i| format!("p2_t{i}")));
        v.push("exc".into());
        v
    } else {
        (0..channels).map(|i| format!("ch{i}")).collect()
    }
}

/// Why a reassembled window was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    TooShort(usize),
    Frame(String),
    NotWindowInfo(u8),
    RunIdMismatch { expected: u32, got: u32 },
    BadGeometry(String),
    DataLength { expected: usize, got: usize },
    DataCrc { expected: u32, got: u32 },
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::TooShort(n) => write!(f, "only {n} bytes, need a 102-byte WindowInfo frame"),
            WindowError::Frame(e) => write!(f, "WindowInfo frame: {e}"),
            WindowError::NotWindowInfo(t) => write!(f, "first frame is type 0x{t:02x}, not WindowInfo"),
            WindowError::RunIdMismatch { expected, got } => {
                write!(f, "WindowInfo run_id {got} != WindowPart run_id {expected}")
            }
            WindowError::BadGeometry(e) => write!(f, "bad geometry: {e}"),
            WindowError::DataLength { expected, got } => {
                write!(f, "data is {got} bytes, WindowInfo says {expected}")
            }
            WindowError::DataCrc { expected, got } => {
                write!(f, "data_crc32 0x{got:08x} != WindowInfo 0x{expected:08x}")
            }
        }
    }
}

impl std::error::Error for WindowError {}

/// Split and verify a reassembled window (WindowInfo frame + data).
/// `run_id` is the WindowPart's run id, if known.
pub fn verify(bytes: &[u8], run_id: Option<u32>) -> Result<(WindowInfo, &[u8]), WindowError> {
    if bytes.len() < WINDOW_INFO_FRAME_LEN {
        return Err(WindowError::TooShort(bytes.len()));
    }
    let frame = board::parse_frame(&bytes[..WINDOW_INFO_FRAME_LEN])
        .map_err(|e| WindowError::Frame(e.to_string()))?;
    let info = match frame.msg {
        BoardMsg::WindowInfo(wi) => wi,
        _ => return Err(WindowError::NotWindowInfo(bytes[1])),
    };
    if let Some(expected) = run_id {
        if info.run_id != expected {
            return Err(WindowError::RunIdMismatch {
                expected,
                got: info.run_id,
            });
        }
    }
    if info.channels == 0 || info.channels > 13 {
        return Err(WindowError::BadGeometry(format!("channels = {}", info.channels)));
    }
    if info.bin_ms_div10 == 0 {
        return Err(WindowError::BadGeometry("bin_ms = 0".into()));
    }
    let data = &bytes[WINDOW_INFO_FRAME_LEN..];
    if data.len() != info.data_len() {
        return Err(WindowError::DataLength {
            expected: info.data_len(),
            got: data.len(),
        });
    }
    let got = crc32(data);
    if got != info.data_crc32 {
        return Err(WindowError::DataCrc {
            expected: info.data_crc32,
            got,
        });
    }
    Ok((info, data))
}

/// Board-monotonic → wall-clock mapping from the window's two stamp pairs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Clock {
    /// RTC milliseconds per monotonic millisecond over the window.
    pub ratio: f64,
    /// Whether wall times are meaningful (RTC valid at both ends).
    pub rtc_valid: bool,
    start_mono_us: u64,
    start_rtc_ms: i64,
}

impl Clock {
    /// Build from a WindowInfo.
    pub fn new(info: &WindowInfo) -> Clock {
        let rtc_valid = info.window_flags & WINDOW_FLAG_TIME_INVALID == 0
            && info.start_rtc_unix_ms >= 0
            && info.end_rtc_unix_ms > info.start_rtc_unix_ms;
        let dmono_ms = info.end_monotonic_us.saturating_sub(info.start_monotonic_us) as f64 / 1000.0;
        let drtc_ms = (info.end_rtc_unix_ms - info.start_rtc_unix_ms) as f64;
        let ratio = if rtc_valid && dmono_ms > 0.0 {
            drtc_ms / dmono_ms
        } else {
            1.0
        };
        Clock {
            ratio,
            rtc_valid,
            start_mono_us: info.start_monotonic_us,
            start_rtc_ms: info.start_rtc_unix_ms,
        }
    }

    /// Wall time (Unix ms) at a board monotonic time.
    pub fn unix_ms(&self, mono_us: f64) -> Option<f64> {
        self.rtc_valid
            .then(|| self.start_rtc_ms as f64 + (mono_us - self.start_mono_us as f64) / 1000.0 * self.ratio)
    }
}

/// One decoded bin.
#[derive(Debug, Clone, PartialEq)]
pub struct Bin {
    /// Bin index.
    pub k: usize,
    /// Monotonic start of the bin.
    pub mono_start_us: u64,
    /// Bin center relative to heater on, board monotonic seconds.
    pub t_s: f64,
    /// Same, rescaled to RTC (crystal) seconds.
    pub t_rtc_s: f64,
    /// Bin center wall time, if the RTC was valid.
    pub unix_ms: Option<f64>,
    /// Mean raw counts per channel; `None` for the sentinel.
    pub raw: Vec<Option<f64>>,
}

/// A verified, decoded window.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub info: WindowInfo,
    pub clock: Clock,
    pub bins: Vec<Bin>,
}

/// Decode window data back to raw counts per channel with timestamps.
pub fn decode(info: &WindowInfo, data: &[u8]) -> Decoded {
    let ch = info.channels as usize;
    let bin_us = info.bin_ms() as u64 * 1000;
    let heater_on_us = info.start_monotonic_us + info.heater_on_offset_ms as u64 * 1000;
    let clock = Clock::new(info);
    let bins = data
        .chunks_exact(ch * 2)
        .enumerate()
        .map(|(k, row)| {
            let mono_start_us = info.start_monotonic_us + k as u64 * bin_us;
            let center = mono_start_us as f64 + bin_us as f64 / 2.0;
            let t_s = (center - heater_on_us as f64) / 1e6;
            let raw = row
                .as_chunks::<2>()
                .0
                .iter()
                .enumerate()
                .map(|(c, v)| {
                    let v = i16::from_le_bytes(*v);
                    decode_bin_value_x16(v, info.base_raw[c]).map(|x16| x16 as f64 / SCALE)
                })
                .collect();
            Bin {
                k,
                mono_start_us,
                t_s,
                t_rtc_s: t_s * clock.ratio,
                unix_ms: clock.unix_ms(center),
                raw,
            }
        })
        .collect();
    Decoded {
        info: *info,
        clock,
        bins,
    }
}

/// Thermistor conversion, same constants as `sap-flux-monitor/src/thermo.rs`
/// (exc → NTC → node → 10 kΩ → GND, ratiometric).
pub fn temp_c(exc: f64, mid: f64) -> Option<f64> {
    const A: [f64; 4] = [0.003354, 2.565e-4, 2.325e-6, 1.024e-7];
    if exc < 100.0 || mid <= 0.0 || mid >= exc {
        return None;
    }
    let r = 10_000.0 * (exc - mid) / mid;
    if !(200.0..=500_000.0).contains(&r) {
        return None;
    }
    let l = (r / 10_000.0).ln();
    let inv_t = A[0] + l * (A[1] + l * (A[2] + l * A[3]));
    if inv_t <= 0.0 {
        return None;
    }
    let t = 1.0 / inv_t - 273.15;
    (-55.0..=150.0).contains(&t).then_some(t)
}

fn fmt_opt(v: Option<f64>, prec: usize) -> String {
    v.map(|x| format!("{x:.prec$}")).unwrap_or_default()
}

/// Write the decoded window as CSV: one row per bin, raw counts (float,
/// empty = no data) and, for the 13-channel layout, temperatures.
pub fn write_csv(d: &Decoded, mut w: impl Write) -> std::io::Result<()> {
    let names = channel_names(d.info.channels);
    let temps = d.info.channels == 13;
    write!(w, "bin,mono_start_us,t_s,t_rtc_s,unix_ms")?;
    for n in &names {
        write!(w, ",{n}_raw")?;
    }
    if temps {
        for n in &names[..12] {
            write!(w, ",{n}_c")?;
        }
    }
    writeln!(w)?;
    for b in &d.bins {
        write!(
            w,
            "{},{},{:.4},{:.6},{}",
            b.k,
            b.mono_start_us,
            b.t_s,
            b.t_rtc_s,
            fmt_opt(b.unix_ms, 1)
        )?;
        for v in &b.raw {
            write!(w, ",{}", fmt_opt(*v, 4))?;
        }
        if temps {
            let exc = b.raw[12];
            for v in &b.raw[..12] {
                let t = match (exc, *v) {
                    (Some(e), Some(m)) => temp_c(e, m),
                    _ => None,
                };
                write!(w, ",{}", fmt_opt(t, 5))?;
            }
        }
        writeln!(w)?;
    }
    Ok(())
}

/// Summary written next to each window as JSON.
#[derive(Debug, Serialize)]
pub struct Summary<'a> {
    pub info: &'a WindowInfo,
    pub flags: Vec<String>,
    pub bin_ms: u32,
    pub channel_names: Vec<String>,
    pub clock_ratio: f64,
    pub rtc_valid: bool,
    pub start_unix_ms: Option<f64>,
    pub heater_on_unix_ms: Option<f64>,
    pub sentinel_values: usize,
}

impl Decoded {
    /// JSON summary.
    pub fn summary(&self) -> Summary<'_> {
        let heater_on = self.info.start_monotonic_us as f64 + self.info.heater_on_offset_ms as f64 * 1000.0;
        Summary {
            info: &self.info,
            flags: board::window_flag_names(self.info.window_flags),
            bin_ms: self.info.bin_ms(),
            channel_names: channel_names(self.info.channels),
            clock_ratio: self.clock.ratio,
            rtc_valid: self.clock.rtc_valid,
            start_unix_ms: self.clock.unix_ms(self.info.start_monotonic_us as f64),
            heater_on_unix_ms: self.clock.unix_ms(heater_on),
            sentinel_values: self
                .bins
                .iter()
                .map(|b| b.raw.iter().filter(|v| v.is_none()).count())
                .sum(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reference binner (what the board does), for the simulator and tests.

/// One raw board frame: 12 thermistors + excitation, `None` = invalid.
#[derive(Debug, Clone, Copy)]
pub struct RawFrame {
    pub monotonic_us: u64,
    pub rtc_unix_ms: i64,
    pub v24_raw: i16,
    pub ch: [Option<i16>; 13],
}

/// Window parameters as the board's scheduler would set them.
#[derive(Debug, Clone, Copy)]
pub struct WindowParams {
    pub run_id: u32,
    pub window_flags: u16,
    /// Monotonic time of heater on.
    pub heater_on_us: u64,
    pub baseline_s: u16,
    pub recovery_s: u16,
    pub pulse_duration_ms: u16,
    pub sample_hz: u16,
    pub bin_ms: u16,
}

/// Bin `frames` into a window: returns the reassembled uplink byte string
/// (WindowInfo frame + data) and the WindowInfo.
///
/// Bin `k` covers `[start + k·bin_ms, start + (k+1)·bin_ms)`; values come
/// from the protocol crate's exact integer `encode_bin_value`, `base_raw[c]`
/// = rounded mean of the first bin (0 if that bin has no valid frames for
/// `c`; the spec leaves this case open). `frames_total` is the expected
/// frame count and `frames_missing` the shortfall, as the crate documents.
pub fn bin_window(frames: &[RawFrame], p: &WindowParams) -> (Vec<u8>, WindowInfo) {
    let start_us = p.heater_on_us - p.baseline_s as u64 * 1_000_000;
    let span_ms = p.baseline_s as u64 * 1000 + p.pulse_duration_ms as u64 + p.recovery_s as u64 * 1000;
    let n_bins = span_ms.div_ceil(p.bin_ms as u64) as usize;
    let bin_us = p.bin_ms as u64 * 1000;
    let end_us = start_us + n_bins as u64 * bin_us;

    let mut sum = vec![[0i32; 13]; n_bins];
    let mut cnt = vec![[0u32; 13]; n_bins];
    let mut frames_total = 0u32;
    let mut v24_sum = 0f64;
    let mut first: Option<&RawFrame> = None;
    let mut last: Option<&RawFrame> = None;
    for f in frames {
        if f.monotonic_us < start_us || f.monotonic_us >= end_us {
            continue;
        }
        let k = ((f.monotonic_us - start_us) / bin_us) as usize;
        frames_total += 1;
        v24_sum += f.v24_raw as f64;
        first.get_or_insert(f);
        last = Some(f);
        for (c, v) in f.ch.iter().enumerate() {
            if let Some(v) = v {
                sum[k][c] += *v as i32;
                cnt[k][c] += 1;
            }
        }
    }
    let mut base = [0i16; 13];
    for (c, b) in base.iter_mut().enumerate() {
        *b = mean_raw_rounded(sum[0][c], cnt[0][c]).unwrap_or(0);
    }
    let mut data = Vec::with_capacity(n_bins * 26);
    for k in 0..n_bins {
        for c in 0..13 {
            data.extend(encode_bin_value(sum[k][c], cnt[k][c], base[c]).to_le_bytes());
        }
    }
    let rtc_at = |f: Option<&RawFrame>, t_us: u64| -> i64 {
        match f {
            Some(f) if f.rtc_unix_ms >= 0 => {
                f.rtc_unix_ms + (t_us as i64 - f.monotonic_us as i64) / 1000
            }
            _ => -1,
        }
    };
    let start_rtc = rtc_at(first, start_us);
    let end_rtc = rtc_at(last, end_us);
    let expected_frames = n_bins as u64 * p.bin_ms as u64 * p.sample_hz as u64 / 1000;
    let mut flags = p.window_flags;
    if start_rtc < 0 || end_rtc < 0 {
        flags |= WINDOW_FLAG_TIME_INVALID;
    }
    let info = WindowInfo {
        run_id: p.run_id,
        window_flags: flags,
        channels: 13,
        bin_ms_div10: (p.bin_ms / 10) as u8,
        n_bins: n_bins as u16,
        chunk_count: data.len().div_ceil(CHUNK_BYTES) as u16,
        start_monotonic_us: start_us,
        start_rtc_unix_ms: start_rtc,
        end_monotonic_us: end_us,
        end_rtc_unix_ms: end_rtc,
        heater_on_offset_ms: p.baseline_s as u32 * 1000,
        pulse_duration_ms: p.pulse_duration_ms,
        sample_hz: p.sample_hz,
        v24_mean_raw: if frames_total > 0 {
            (v24_sum / frames_total as f64).round() as i16
        } else {
            0
        },
        frames_total: expected_frames.min(u16::MAX as u64) as u16,
        frames_missing: expected_frames.saturating_sub(frames_total as u64).min(u16::MAX as u64) as u16,
        data_crc32: crc32(&data),
        base_raw: base,
    };
    let mut bytes = board::window_info_frame(&info, 1);
    bytes.extend_from_slice(&data);
    (bytes, info)
}

/// Read a `sap-flux-monitor` capture (`samples.csv`) into raw frames.
/// The capture has one probe (T1..T6), mapped to probe 1; probe 2 is
/// absent (`None`). A frame is invalid when `sample_valid` is 0 or
/// `adc_error` is 1. Returns the frames and the monotonic time of the first
/// heater-on sample of the run with the most heater-on samples.
pub fn read_capture(path: &std::path::Path) -> anyhow::Result<(Vec<RawFrame>, Option<u64>)> {
    use anyhow::Context;
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().context("empty capture")?.split(',').collect();
    let col = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .with_context(|| format!("capture has no column {name}"))
    };
    let mono = col("monotonic_us")?;
    let rtc = col("rtc_unix_ms")?;
    let run = col("run_id")?;
    let heat = col("heater_active")?;
    let valid = col("sample_valid")?;
    let adc_err = col("adc_error")?;
    let exc = col("excitation_raw")?;
    let v24 = col("v24_raw")?;
    let t: Vec<usize> = (1..=6).map(|i| col(&format!("t{i}_raw"))).collect::<Result<_, _>>()?;
    let mut frames = Vec::new();
    let mut heater: std::collections::HashMap<&str, (u32, u64)> = Default::default();
    for line in lines {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < header.len() {
            continue;
        }
        let m: u64 = f[mono].parse()?;
        let ok = f[valid] == "1" && f[adc_err] == "0";
        let mut ch = [None; 13];
        if ok {
            for (i, &c) in t.iter().enumerate() {
                ch[i] = f[c].parse().ok();
            }
            ch[12] = f[exc].parse().ok();
        }
        if f[heat] == "1" {
            let e = heater.entry(f[run]).or_insert((0, m));
            e.0 += 1;
            e.1 = e.1.min(m);
        }
        frames.push(RawFrame {
            monotonic_us: m,
            rtc_unix_ms: f[rtc].parse()?,
            v24_raw: f[v24].parse().unwrap_or(0),
            ch,
        });
    }
    let onset = heater.values().max_by_key(|(n, _)| *n).map(|(_, m)| *m);
    Ok((frames, onset))
}

/// Keep every `n`-th frame (e.g. 50 Hz capture → 25 Hz board rate).
pub fn decimate(frames: &[RawFrame], n: usize) -> Vec<RawFrame> {
    frames.iter().step_by(n.max(1)).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(heater_on_us: u64) -> Vec<RawFrame> {
        // 25 Hz, 250 s around the pulse, ramps so every bin differs.
        (0..25 * 250)
            .map(|i| {
                let m = heater_on_us - 65_000_000 + i as u64 * 40_000;
                let mut ch = [None; 13];
                for (c, v) in ch.iter_mut().enumerate() {
                    *v = Some(11_000 + c as i16 * 10 + (i / 25) as i16);
                }
                ch[12] = Some(24_880);
                if (2000..2100).contains(&i) {
                    ch[11] = None;
                }
                RawFrame {
                    monotonic_us: m,
                    rtc_unix_ms: 1_787_618_000_000 + (m / 1000) as i64,
                    v24_raw: 3000,
                    ch,
                }
            })
            .collect()
    }

    #[test]
    fn bin_verify_decode() {
        let p = WindowParams {
            run_id: 9,
            window_flags: 0,
            heater_on_us: 100_000_000,
            baseline_s: 60,
            recovery_s: 180,
            pulse_duration_ms: 1000,
            sample_hz: 25,
            bin_ms: 500,
        };
        let frames = synth(p.heater_on_us);
        let (bytes, info) = bin_window(&frames, &p);
        assert_eq!(info.n_bins, 482);
        assert_eq!(info.chunk_count, 126);
        assert_eq!(bytes.len(), 102 + 12532);
        assert_eq!(info.frames_total, 6025); // 241 s at 25 Hz
        assert_eq!(info.frames_missing, 0);
        let (vi, data) = verify(&bytes, Some(9)).unwrap();
        assert_eq!(vi, info);
        let d = decode(&vi, data);
        assert_eq!(d.bins.len(), 482);
        assert!((d.clock.ratio - 1.0).abs() < 1e-9);
        // Bin 120 is the 0.5 s bin starting at heater on: center t = +0.25 s.
        assert!((d.bins[120].t_s - 0.25).abs() < 1e-9);
        // Decoded raw = exact bin mean within 1/32 count.
        for b in &d.bins {
            let lo = p.heater_on_us - 60_000_000 + b.k as u64 * 500_000;
            let fs: Vec<_> = frames
                .iter()
                .filter(|f| f.monotonic_us >= lo && f.monotonic_us < lo + 500_000)
                .collect();
            for c in 0..13 {
                let vals: Vec<f64> = fs.iter().filter_map(|f| f.ch[c]).map(|v| v as f64).collect();
                match b.raw[c] {
                    None => assert!(vals.is_empty(), "bin {} ch {c}", b.k),
                    Some(x) => {
                        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
                        assert!((x - mean).abs() <= 1.0 / 32.0 + 1e-9, "bin {} ch {c}", b.k);
                    }
                }
            }
        }
        assert!(d.summary().sentinel_values > 0);
        let mut csv = Vec::new();
        write_csv(&d, &mut csv).unwrap();
        let csv = String::from_utf8(csv).unwrap();
        assert_eq!(csv.lines().count(), 483);
        assert!(csv.starts_with("bin,mono_start_us,t_s,t_rtc_s,unix_ms,p1_t1_raw"));

        let mut bad = bytes.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(matches!(verify(&bad, Some(9)), Err(WindowError::DataCrc { .. })));
        assert!(matches!(verify(&bytes, Some(8)), Err(WindowError::RunIdMismatch { .. })));
        assert!(matches!(verify(&bytes[..bytes.len() - 2], None), Err(WindowError::DataLength { .. })));
    }

    #[test]
    fn thermistor_midpoint_is_25c() {
        let t = temp_c(26000.0, 13000.0).unwrap();
        assert!((t - (1.0 / 0.003354 - 273.15)).abs() < 1e-9);
        assert!(temp_c(26000.0, 0.0).is_none());
    }
}
