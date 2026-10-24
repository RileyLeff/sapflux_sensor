//! Window download, verification and export.

use crate::link::{Link, LinkError, Transport};
use sap_flux_protocol::field::{chunk_range, decode_bin_value, read_bin_value};
use sap_flux_protocol::{
    encode_frame, Message, WindowChunk, WindowInfo, MAX_FRAME_LEN, WINDOW_CHANNELS,
    WINDOW_FLAG_ABORTED, WINDOW_FLAG_ADC_ERRORS, WINDOW_FLAG_HEATER_FAULT,
    WINDOW_FLAG_TIME_INVALID, WINDOW_FLAG_TRUNCATED,
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
    /// Window data (`n_bins × channels × i16 LE`).
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
    const NAMES: [(u16, &str); 5] = [
        (WINDOW_FLAG_TIME_INVALID, "TIME_INVALID"),
        (WINDOW_FLAG_ADC_ERRORS, "ADC_ERRORS"),
        (WINDOW_FLAG_ABORTED, "ABORTED"),
        (WINDOW_FLAG_HEATER_FAULT, "HEATER_FAULT"),
        (WINDOW_FLAG_TRUNCATED, "TRUNCATED"),
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
        Ok(())
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

    /// Human summary of the header.
    pub fn summary(&self) -> String {
        let i = &self.info;
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
