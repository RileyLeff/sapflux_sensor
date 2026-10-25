//! Window download, verification and export.

use crate::link::{Link, LinkError, Transport};
use sap_flux_protocol::field::{chunk_range, decode_bin_value, read_bin_value, sfk1_records};
use sap_flux_protocol::{
    encode_frame, Message, WindowChunk, WindowInfo, MAX_FRAME_LEN, WINDOW_CHANNELS,
    WINDOW_FLAG_ABORTED, WINDOW_FLAG_ADC_ERRORS, WINDOW_FLAG_HEATER_FAULT,
    WINDOW_FLAG_SFK1, WINDOW_FLAG_TIME_INVALID, WINDOW_FLAG_TRUNCATED,
};
use std::io::{self, Write};

/// CSV names of the 13 window channels, matching the monitor's
/// `samples.csv` naming (probe 1 `t1`..`t6`, probe 2 `p2t1`..`p2t6`).
pub const CHANNEL_COLUMNS: [&str; WINDOW_CHANNELS] = [
    "t1_raw",
    "t2_raw",
    "t3_raw",
    "t4_raw",
    "t5_raw",
    "t6_raw",
    "p2t1_raw",
    "p2t2_raw",
    "p2t3_raw",
    "p2t4_raw",
    "p2t5_raw",
    "p2t6_raw",
    "excitation_raw",
];

/// A downloaded window: header plus the reassembled data bytes.
#[derive(Debug, Clone)]
pub struct Window {
    /// The `WindowInfo` header.
    pub info: WindowInfo,
    /// Window data (`n_bins × channels × i16 LE`, or SFK1 entries).
    pub data: Vec<u8>,
}

/// Why a window could not be assembled or verified.
#[derive(Debug)]
pub enum WindowError {
    /// The link failed.
    Link(LinkError),
    /// The data does not match the header.
    Verify(String),
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::Link(e) => write!(f, "{e}"),
            WindowError::Verify(s) => write!(f, "verification failed: {s}"),
        }
    }
}

impl std::error::Error for WindowError {}

impl From<LinkError> for WindowError {
    fn from(e: LinkError) -> Self {
        WindowError::Link(e)
    }
}

/// Names of the set `window_flags` bits, `|`-joined (`"-"` when none).
pub fn window_flag_names(flags: u16) -> String {
    const NAMES: [(u16, &str); 6] = [
        (WINDOW_FLAG_TIME_INVALID, "TIME_INVALID"),
        (WINDOW_FLAG_ADC_ERRORS, "ADC_ERRORS"),
        (WINDOW_FLAG_ABORTED, "ABORTED"),
        (WINDOW_FLAG_HEATER_FAULT, "HEATER_FAULT"),
        (WINDOW_FLAG_TRUNCATED, "TRUNCATED"),
        (WINDOW_FLAG_SFK1, "SFK1"),
    ];
    let set: Vec<&str> = NAMES
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if set.is_empty() {
        "-".into()
    } else {
        set.join("|")
    }
}

impl Window {
    /// Assemble from a header and its chunks (any order), checking every
    /// chunk's run id, index and length, then the data CRC.
    pub fn assemble(info: WindowInfo, chunks: &[WindowChunk]) -> Result<Self, WindowError> {
        let data_len = info.data_len();
        if info.chunk_count as usize != info.expected_chunk_count() {
            return Err(WindowError::Verify(format!(
                "header chunk_count {} but {} data bytes need {}",
                info.chunk_count,
                data_len,
                info.expected_chunk_count()
            )));
        }
        let mut data = vec![0u8; data_len];
        let mut seen = vec![false; info.chunk_count as usize];
        for c in chunks {
            if c.run_id != info.run_id {
                return Err(WindowError::Verify(format!(
                    "chunk {} has run_id {}, expected {}",
                    c.chunk, c.run_id, info.run_id
                )));
            }
            let Some(range) = chunk_range(c.chunk, data_len) else {
                return Err(WindowError::Verify(format!(
                    "chunk {} out of range",
                    c.chunk
                )));
            };
            if c.len as usize != range.len() {
                return Err(WindowError::Verify(format!(
                    "chunk {} has len {}, expected {}",
                    c.chunk,
                    c.len,
                    range.len()
                )));
            }
            data[range].copy_from_slice(c.bytes());
            seen[c.chunk as usize] = true;
        }
        if let Some(missing) = seen.iter().position(|s| !s) {
            return Err(WindowError::Verify(format!("chunk {missing} missing")));
        }
        let w = Self { info, data };
        w.verify()?;
        Ok(w)
    }

    /// Check data length and `data_crc32`.
    pub fn verify(&self) -> Result<(), WindowError> {
        if self.data.len() != self.info.data_len() {
            return Err(WindowError::Verify(format!(
                "data is {} bytes, header implies {}",
                self.data.len(),
                self.info.data_len()
            )));
        }
        if !self.info.verify_data(&self.data) {
            return Err(WindowError::Verify(format!(
                "data_crc32 mismatch: header {:08x}, computed {:08x}",
                self.info.data_crc32,
                sap_flux_protocol::crc::crc32(&self.data)
            )));
        }
        if self.info.is_sfk1() {
            if let Some(Err(e)) = sfk1_records(&self.data).find(Result::is_err) {
                return Err(WindowError::Verify(format!(
                    "SFK1 entry at byte {} runs past the data",
                    e.offset
                )));
            }
        }
        Ok(())
    }

    /// An SFK1 window's records: `(probe, record length, decoded record or
    /// why it failed)`. An empty record (the board had none) decodes to
    /// `None`.
    pub fn sfk1_records(&self) -> Vec<(u8, usize, Result<Option<sfk1::Decoded>, String>)> {
        sfk1_records(&self.data)
            .map_while(Result::ok)
            .map(|(probe, rec)| {
                let d = match rec {
                    [] => Ok(None),
                    rec => sfk1::decode(rec).map(Some).map_err(|e| e.to_string()),
                };
                (probe, rec.len(), d)
            })
            .collect()
    }

    /// The byte string the gateway uploads in `WindowPart`s: the decoded
    /// `WindowInfo` frame (102 B, sequence 0) followed by the data.
    pub fn uplink_bytes(&self) -> Vec<u8> {
        let mut frame = [0u8; MAX_FRAME_LEN];
        let n = encode_frame(&Message::WindowInfo(self.info), 0, &mut frame)
            .expect("WindowInfo fits a frame");
        let mut out = frame[..n].to_vec();
        out.extend_from_slice(&self.data);
        out
    }

    /// RTC time (Unix ms) of a monotonic instant, interpolated between the
    /// window's start and end stamps. `None` when the window's time is
    /// invalid.
    pub fn rtc_at(&self, monotonic_us: u64) -> Option<f64> {
        let i = &self.info;
        if i.window_flags & WINDOW_FLAG_TIME_INVALID != 0
            || i.start_rtc_unix_ms < 0
            || i.end_rtc_unix_ms < 0
        {
            return None;
        }
        let dt_us = monotonic_us as f64 - i.start_monotonic_us as f64;
        let span_us = i.end_monotonic_us as f64 - i.start_monotonic_us as f64;
        let scale = if span_us > 0.0 {
            (i.end_rtc_unix_ms - i.start_rtc_unix_ms) as f64 / (span_us / 1000.0)
        } else {
            1.0
        };
        Some(i.start_rtc_unix_ms as f64 + dt_us / 1000.0 * scale)
    }

    /// Write the decoded window as CSV: one row per bin with bin start
    /// times and the 13 channels converted back to (fractional) raw ADC
    /// counts. Missing values are empty.
    pub fn write_csv(&self, mut w: impl Write) -> io::Result<()> {
        if self.info.is_sfk1() {
            return self.write_sfk1_csv(w);
        }
        let i = &self.info;
        let channels = i.channels as usize;
        write!(
            w,
            "bin,bin_start_offset_ms,t_from_heater_on_s,bin_start_monotonic_us,bin_start_rtc_unix_ms"
        )?;
        for c in 0..channels {
            match CHANNEL_COLUMNS.get(c) {
                Some(name) => write!(w, ",{name}")?,
                None => write!(w, ",ch{c}_raw")?,
            }
        }
        writeln!(w)?;
        let bin_ms = i.bin_ms() as u64;
        for k in 0..i.n_bins as usize {
            let offset_ms = k as u64 * bin_ms;
            let mono = i.bin_start_monotonic_us(k as u32);
            let t_heater = (offset_ms as f64 - i.heater_on_offset_ms as f64) / 1000.0;
            write!(w, "{k},{offset_ms},{t_heater:.3},{mono},")?;
            if let Some(rtc) = self.rtc_at(mono) {
                write!(w, "{}", rtc.round() as i64)?;
            }
            for c in 0..channels {
                let base = i.base_raw.get(c).copied().unwrap_or(0);
                let v = read_bin_value(&self.data, k, c, channels)
                    .and_then(|v| decode_bin_value(v, base));
                match v {
                    // 1/16-count resolution is exact with 4 decimals.
                    Some(raw) => write!(w, ",{raw:.4}")?,
                    None => write!(w, ",")?,
                }
            }
            writeln!(w)?;
        }
        Ok(())
    }

    /// CSV of an SFK1 window: one row per record frame (40 ms apart from
    /// the window start) with the 13 channel columns. Thermistors are the
    /// records' rebuilt curves in counts at the first record's reference
    /// excitation (another probe's rescaled to it, the measurement being
    /// ratiometric), `excitation_raw` is that reference, and a probe or
    /// thermistor without a record is empty.
    fn write_sfk1_csv(&self, mut w: impl Write) -> io::Result<()> {
        let i = &self.info;
        let recs: Vec<(u8, sfk1::Decoded)> = self
            .sfk1_records()
            .into_iter()
            .filter_map(|(p, _, d)| Some((p, d.ok()??)))
            .collect();
        write!(
            w,
            "frame,frame_offset_ms,t_from_heater_on_s,frame_monotonic_us,frame_rtc_unix_ms"
        )?;
        for name in CHANNEL_COLUMNS {
            write!(w, ",{name}")?;
        }
        writeln!(w)?;
        let n_frames = recs
            .iter()
            .map(|(_, d)| d.channels.first().map_or(0, |c| c.samples.len()))
            .max()
            .unwrap_or(0);
        let e_ref = recs.first().map(|(_, d)| f64::from(d.e_ref));
        let period_us = recs.first().map_or(40_000, |(_, d)| d.frame_period_us) as u64;
        for k in 0..n_frames {
            let offset_us = k as u64 * period_us;
            let mono = i.start_monotonic_us + offset_us;
            let t_heater = (offset_us as f64 / 1000.0 - i.heater_on_offset_ms as f64) / 1000.0;
            write!(w, "{k},{:.0},{t_heater:.3},{mono},", offset_us as f64 / 1000.0)?;
            if let Some(rtc) = self.rtc_at(mono) {
                write!(w, "{}", rtc.round() as i64)?;
            }
            let mut row = [None; WINDOW_CHANNELS];
            for (probe, d) in &recs {
                let scale = e_ref.map_or(1.0, |e| e / f64::from(d.e_ref.max(1)));
                for ch in &d.channels {
                    let c = (*probe as usize - 1) * 6 + ch.channel as usize - 1;
                    if (1..=6).contains(&ch.channel) && c < 12 {
                        row[c] = ch.samples.get(k).map(|v| v * scale);
                    }
                }
            }
            row[12] = e_ref;
            for v in row {
                match v {
                    Some(x) => write!(w, ",{x:.4}")?,
                    None => write!(w, ",")?,
                }
            }
            writeln!(w)?;
        }
        Ok(())
    }

    /// Human summary of the header.
    pub fn summary(&self) -> String {
        let i = &self.info;
        if i.is_sfk1() {
            let mut s = format!(
                "run {} flags {} ({}) SFK1, {} chunks, {} data bytes, crc32 {:08x}\n\
                 start mono {} us rtc {} ms; end mono {} us rtc {} ms\n\
                 heater on at +{} ms for {} ms, {} Hz, frames {} total / {} missing, v24_mean_raw {}",
                i.run_id,
                i.window_flags,
                window_flag_names(i.window_flags),
                i.chunk_count,
                i.data_len(),
                i.data_crc32,
                i.start_monotonic_us,
                i.start_rtc_unix_ms,
                i.end_monotonic_us,
                i.end_rtc_unix_ms,
                i.heater_on_offset_ms,
                i.pulse_duration_ms,
                i.sample_hz,
                i.frames_total,
                i.frames_missing,
                i.v24_mean_raw
            );
            for (probe, len, d) in self.sfk1_records() {
                s += &match d {
                    Ok(Some(d)) => format!(
                        "\nprobe {probe}: {len} B {}, E_ref {}, thermistors {:?}",
                        d.profile,
                        d.e_ref,
                        d.channels.iter().map(|c| c.channel).collect::<Vec<_>>()
                    ),
                    Ok(None) => format!("\nprobe {probe}: no record"),
                    Err(e) => format!("\nprobe {probe}: {len} B, does not decode: {e}"),
                };
            }
            return s;
        }
        format!(
            "run {} flags {} ({}) bins {}x{} @ {} ms, {} chunks, {} data bytes, crc32 {:08x}\n\
             start mono {} us rtc {} ms; end mono {} us rtc {} ms\n\
             heater on at +{} ms for {} ms, {} Hz, frames {} total / {} missing, v24_mean_raw {}",
            i.run_id,
            i.window_flags,
            window_flag_names(i.window_flags),
            i.n_bins,
            i.channels,
            i.bin_ms(),
            i.chunk_count,
            i.data_len(),
            i.data_crc32,
            i.start_monotonic_us,
            i.start_rtc_unix_ms,
            i.end_monotonic_us,
            i.end_rtc_unix_ms,
            i.heater_on_offset_ms,
            i.pulse_duration_ms,
            i.sample_hz,
            i.frames_total,
            i.frames_missing,
            i.v24_mean_raw
        )
    }
}

/// Download window `run_id` (0 = oldest undelivered): header, then every
/// chunk in order, then verify. Chunks are requested by the header's real
/// run id so the oldest window cannot change underneath the download.
/// `progress` is called after each chunk with `(done, total)`.
pub fn download<T: Transport>(
    link: &mut Link<T>,
    run_id: u32,
    mut progress: impl FnMut(usize, usize),
) -> Result<Window, WindowError> {
    let info = link.window_info(run_id)?;
    if info.chunk_count as usize != info.expected_chunk_count() {
        return Err(WindowError::Verify(format!(
            "header chunk_count {} inconsistent with n_bins {} x channels {}",
            info.chunk_count, info.n_bins, info.channels
        )));
    }
    let total = info.chunk_count as usize;
    let mut chunks = Vec::with_capacity(total);
    for c in 0..info.chunk_count {
        chunks.push(link.window_chunk(info.run_id, c)?);
        progress(c as usize + 1, total);
    }
    Window::assemble(info, &chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sap_flux_protocol::field::write_sfk1_prefix;

    fn record(exc: i16) -> Vec<u8> {
        let mut enc = sfk1::Encoder::<6, sfk1::East30K12>::new(&[0b11_1111], 12).unwrap();
        let onset = enc.onset_frame();
        for n in 0..enc.frames_expected() {
            let v = if n >= onset { 12_700 } else { 12_500 };
            enc.push(exc, &[v; 6]).unwrap();
        }
        let mut out = vec![0u8; sfk1::Encoder::<6, sfk1::East30K12>::MAX_RECORD_LEN];
        let len = enc.finish(&mut out).unwrap().len;
        out.truncate(len);
        out
    }

    #[test]
    fn sfk1_window_csv_and_summary() {
        let rec = record(25_000);
        let mut data = Vec::new();
        for (probe, r) in [(1u8, &rec[..]), (2, &[][..])] {
            let mut p = [0u8; 3];
            write_sfk1_prefix(&mut p, probe, r.len() as u16);
            data.extend(p);
            data.extend_from_slice(r);
        }
        let info = WindowInfo {
            run_id: 3,
            window_flags: WINDOW_FLAG_SFK1,
            channels: WINDOW_CHANNELS as u8,
            n_bins: data.len() as u16,
            chunk_count: data.len().div_ceil(sap_flux_protocol::WINDOW_CHUNK_DATA_LEN) as u16,
            heater_on_offset_ms: 60_000,
            data_crc32: sap_flux_protocol::crc::crc32(&data),
            bin_ms_div10: 0,
            start_monotonic_us: 0,
            start_rtc_unix_ms: -1,
            end_monotonic_us: 241_000_000,
            end_rtc_unix_ms: -1,
            pulse_duration_ms: 1000,
            sample_hz: 25,
            v24_mean_raw: 0,
            frames_total: 6025,
            frames_missing: 0,
            base_raw: [0; WINDOW_CHANNELS],
        };
        let w = Window { info, data };
        w.verify().unwrap();
        assert!(w.summary().contains("probe 2: no record"));
        let mut csv = Vec::new();
        w.write_csv(&mut csv).unwrap();
        let csv = String::from_utf8(csv).unwrap();
        let mut lines = csv.lines();
        assert!(lines.next().unwrap().starts_with("frame,frame_offset_ms,t_from_heater_on_s"));
        let row: Vec<&str> = lines.nth(1500 + 250).unwrap().split(',').collect();
        assert_eq!(row[2], "10.000");
        let t1: f64 = row[5].parse().unwrap();
        assert!((t1 - 12_700.0).abs() < 3.0, "{t1}");
        assert_eq!(row[11], ""); // probe 2 T1
        assert_eq!(row[17], "25000.0000");
    }
}
