//! Capture-session logging.
//!
//! A session directory is created under the captures root on the first
//! valid Hello or Sample after connect, and receives:
//!
//! - `raw.cobs` — exact received serial bytes, append-only (bytes that
//!   arrived before the first valid frame are kept in a bounded backlog and
//!   written at session start);
//! - `samples.csv` — one row per successfully decoded Sample or Sample12
//!   (probe-2 columns are empty for 6-channel `Sample` frames);
//! - `events.csv` — everything else: Hello/Status/Ack/Error/FieldStatus/
//!   WindowInfo/WindowChunk, connection
//!   transitions, parser errors, sequence gaps, outbound commands;
//! - `metadata.json` — app/protocol versions, port settings, board info,
//!   channel labels, conversion coefficients, session times, final counters.
//!
//! Logging happens in the serial worker *before* UI delivery, so a slow UI
//! can never lose logged frames. Files are flushed at least once per second
//! and immediately on status transitions and outbound commands.

use crate::channels::CHANNELS;
use crate::stats::StatsSnapshot;
use crate::thermo;
use crate::types::{state_name, ProcessedSample};
use sap_flux_protocol::{
    Hello, FLAG_ADC_ERROR, FLAG_EXCITATION_ACTIVE, FLAG_HEATER_ACTIVE, FLAG_PGOOD, FLAG_RTC_VALID,
    PROTOCOL_VERSION,
};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Bytes of pre-session raw serial data retained.
const RAW_BACKLOG_CAP: usize = 256 * 1024;
/// Pre-session event rows retained.
const EVENT_BACKLOG_CAP: usize = 2000;
/// Background flush interval.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Connection settings recorded in metadata.
#[derive(Debug, Clone)]
pub struct LoggerConfig {
    /// Directory sessions are created under.
    pub captures_root: PathBuf,
    /// Port name, or "simulator".
    pub port_name: String,
    /// Baud rate (0 for simulator).
    pub baud: u32,
    /// True when fed by the simulator instead of a serial port.
    pub simulated: bool,
}

#[derive(Serialize)]
struct ChannelMeta {
    id: &'static str,
    probe: u8,
    wire_color: &'static str,
    position: &'static str,
    known_broken: bool,
}

#[derive(Serialize)]
struct ThermistorMeta {
    formula: &'static str,
    coeffs_low_order_first: [f64; 4],
    r_ref_ohm: f64,
}

#[derive(Serialize)]
struct CountersMeta {
    bytes_rx: u64,
    bytes_tx: u64,
    frames_ok: u64,
    samples_ok: u64,
    crc_errors: u64,
    framing_errors: u64,
    invalid_samples: u64,
    seq_gaps: u64,
    seq_missing: u64,
    ui_events_dropped: u64,
    sample_rows_written: u64,
    event_rows_written: u64,
}

#[derive(Serialize)]
struct Metadata {
    app: &'static str,
    app_version: &'static str,
    protocol_version: u8,
    port: String,
    baud: u32,
    simulated: bool,
    session_start_unix_ns: i64,
    session_start_iso: String,
    session_end_unix_ns: Option<i64>,
    session_end_iso: Option<String>,
    board_id_hex: Option<String>,
    firmware_version: Option<String>,
    capabilities: Option<u32>,
    channels: Vec<ChannelMeta>,
    thermistor: ThermistorMeta,
    raw_backlog_truncated: bool,
    final_counters: Option<CountersMeta>,
}

struct ActiveSession {
    dir: PathBuf,
    raw: BufWriter<File>,
    samples: BufWriter<File>,
    events: BufWriter<File>,
    meta: Metadata,
    last_flush: Instant,
    dirty: bool,
    sample_rows: u64,
    event_rows: u64,
}

/// Session logger. Every method records failures in
/// [`last_error`](Self::last_error) instead of panicking; the worker
/// surfaces that in the UI.
pub struct Logger {
    cfg: LoggerConfig,
    session: Option<ActiveSession>,
    raw_backlog: Vec<u8>,
    event_backlog: Vec<String>,
    raw_backlog_truncated: bool,
    board: Option<Hello>,
    /// Most recent I/O error, if any.
    pub last_error: Option<String>,
}

impl Logger {
    /// Create a logger; no files are touched until
    /// [`ensure_session`](Self::ensure_session).
    pub fn new(cfg: LoggerConfig) -> Self {
        Self {
            cfg,
            session: None,
            raw_backlog: Vec::new(),
            event_backlog: Vec::new(),
            raw_backlog_truncated: false,
            board: None,
            last_error: None,
        }
    }

    /// Directory of the active session, if one has started.
    pub fn session_dir(&self) -> Option<&Path> {
        self.session.as_ref().map(|s| s.dir.as_path())
    }

    /// Rows written to samples.csv so far.
    pub fn sample_rows(&self) -> u64 {
        self.session.as_ref().map_or(0, |s| s.sample_rows)
    }

    /// Record received serial bytes exactly as they arrived.
    pub fn on_raw(&mut self, bytes: &[u8]) {
        match &mut self.session {
            Some(s) => {
                if let Err(e) = s.raw.write_all(bytes) {
                    self.last_error = Some(format!("raw.cobs write: {e}"));
                } else {
                    s.dirty = true;
                }
            }
            None => {
                let room = RAW_BACKLOG_CAP.saturating_sub(self.raw_backlog.len());
                if room >= bytes.len() {
                    self.raw_backlog.extend_from_slice(bytes);
                } else {
                    self.raw_backlog.extend_from_slice(&bytes[..room]);
                    self.raw_backlog_truncated = true;
                }
            }
        }
    }

    /// Record board identity for metadata.
    pub fn set_board_info(&mut self, hello: &Hello) {
        self.board = Some(*hello);
        let dir = match &mut self.session {
            Some(s) => {
                s.meta.board_id_hex = Some(format!("{:016X}", hello.board_id));
                s.meta.firmware_version =
                    Some(crate::types::format_fw_version(hello.firmware_version));
                s.meta.capabilities = Some(hello.capabilities);
                Some(s.dir.clone())
            }
            None => None,
        };
        if let Some(dir) = dir {
            self.write_metadata(&dir);
        }
    }

    /// Append a row to events.csv (or the pre-session backlog).
    pub fn log_event(&mut self, host_unix_ns: i64, kind: &str, detail: &str) {
        let row = format!("{host_unix_ns},{kind},{}\n", csv_escape(detail));
        match &mut self.session {
            Some(s) => {
                if let Err(e) = s.events.write_all(row.as_bytes()) {
                    self.last_error = Some(format!("events.csv write: {e}"));
                } else {
                    s.event_rows += 1;
                    s.dirty = true;
                }
            }
            None => {
                if self.event_backlog.len() < EVENT_BACKLOG_CAP {
                    self.event_backlog.push(row);
                }
            }
        }
    }

    /// Append one row to samples.csv. Does nothing before session start
    /// (the caller opens the session on the first valid Sample/Hello, so in
    /// practice every decoded sample is logged).
    pub fn log_sample(&mut self, ps: &ProcessedSample) {
        let Some(s) = &mut self.session else { return };
        let smp = &ps.sample;
        let f = smp.flags;
        let mut row = String::with_capacity(256);
        use std::fmt::Write as _;
        let _ = write!(
            row,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            ps.host_unix_ns,
            ps.sequence,
            smp.monotonic_us,
            smp.rtc_unix_ms,
            smp.run_id,
            f,
            (f & FLAG_HEATER_ACTIVE != 0) as u8,
            (f & FLAG_RTC_VALID != 0) as u8,
            (f & FLAG_PGOOD != 0) as u8,
            (f & FLAG_EXCITATION_ACTIVE != 0) as u8,
            (f & FLAG_ADC_ERROR != 0) as u8,
            smp.state as u8,
            state_name(smp.state),
            ps.sample_valid as u8,
            smp.excitation_raw,
            ps.exc_valid as u8,
            smp.v24_raw,
        );
        let _ = write!(row, ",{}", ps.channel_count());
        for (i, r) in ps.readings.iter().enumerate() {
            if i >= ps.channel_count() {
                // Channel not carried by this frame type.
                row.push_str(",,,,");
                continue;
            }
            let _ = write!(row, ",{}", r.raw);
            match r.resistance_ohm {
                Some(res) => {
                    let _ = write!(row, ",{res:.3}");
                }
                None => row.push(','),
            }
            match r.temp_c {
                Some(t) => {
                    let _ = write!(row, ",{t:.4}");
                }
                None => row.push(','),
            }
            let _ = write!(row, ",{}", r.valid() as u8);
        }
        row.push('\n');
        if let Err(e) = s.samples.write_all(row.as_bytes()) {
            self.last_error = Some(format!("samples.csv write: {e}"));
        } else {
            s.sample_rows += 1;
            s.dirty = true;
        }
    }

    /// Create the session directory and files if not already active.
    /// Returns the directory when a new session was just created.
    pub fn ensure_session(&mut self, host_unix_ns: i64, now: Instant) -> Option<PathBuf> {
        if self.session.is_some() {
            return None;
        }
        match self.open_session(host_unix_ns, now) {
            Ok(dir) => Some(dir),
            Err(e) => {
                self.last_error = Some(format!("session create: {e}"));
                None
            }
        }
    }

    fn open_session(&mut self, host_unix_ns: i64, now: Instant) -> std::io::Result<PathBuf> {
        fs::create_dir_all(&self.cfg.captures_root)?;
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let mut dir = self.cfg.captures_root.join(format!("session_{stamp}"));
        // Avoid collisions if two sessions start within a second.
        let mut n = 1;
        while dir.exists() {
            dir = self.cfg.captures_root.join(format!("session_{stamp}_{n}"));
            n += 1;
        }
        fs::create_dir_all(&dir)?;

        let open = |name: &str| -> std::io::Result<BufWriter<File>> {
            Ok(BufWriter::new(
                OpenOptions::new()
                    .create_new(true)
                    .append(true)
                    .open(dir.join(name))?,
            ))
        };
        let mut raw = open("raw.cobs")?;
        let mut samples = open("samples.csv")?;
        let mut events = open("events.csv")?;

        samples.write_all(samples_csv_header().as_bytes())?;
        events.write_all(b"host_unix_ns,kind,detail\n")?;

        // Replay pre-session backlogs.
        raw.write_all(&self.raw_backlog)?;
        self.raw_backlog = Vec::new();
        for row in self.event_backlog.drain(..) {
            events.write_all(row.as_bytes())?;
        }

        let meta = Metadata {
            app: "sap-flux-monitor",
            app_version: env!("CARGO_PKG_VERSION"),
            protocol_version: PROTOCOL_VERSION,
            port: self.cfg.port_name.clone(),
            baud: self.cfg.baud,
            simulated: self.cfg.simulated,
            session_start_unix_ns: host_unix_ns,
            session_start_iso: chrono::Local::now().to_rfc3339(),
            session_end_unix_ns: None,
            session_end_iso: None,
            board_id_hex: self.board.map(|h| format!("{:016X}", h.board_id)),
            firmware_version: self
                .board
                .map(|h| crate::types::format_fw_version(h.firmware_version)),
            capabilities: self.board.map(|h| h.capabilities),
            channels: CHANNELS
                .iter()
                .map(|c| ChannelMeta {
                    id: c.id,
                    probe: c.probe,
                    wire_color: c.wire_color,
                    position: c.position,
                    known_broken: !self.cfg.simulated && c.known_broken,
                })
                .collect(),
            thermistor: ThermistorMeta {
                formula: "R_ntc = r_ref*(exc_raw-mid_raw)/mid_raw; l = ln(R_ntc/r_ref); \
                          T_C = 1/(c0 + c1*l + c2*l^2 + c3*l^3) - 273.15",
                coeffs_low_order_first: thermo::COEFFS,
                r_ref_ohm: thermo::R_REF_OHM,
            },
            raw_backlog_truncated: self.raw_backlog_truncated,
            final_counters: None,
        };

        self.session = Some(ActiveSession {
            dir: dir.clone(),
            raw,
            samples,
            events,
            meta,
            last_flush: now,
            dirty: true,
            sample_rows: 0,
            event_rows: 0,
        });
        self.log_event(host_unix_ns, "session_start", &dir.display().to_string());
        self.write_metadata(&dir);
        self.force_flush();
        Ok(dir)
    }

    fn write_metadata(&mut self, dir: &Path) {
        let Some(s) = &self.session else { return };
        match serde_json::to_vec_pretty(&s.meta) {
            Ok(json) => {
                if let Err(e) = fs::write(dir.join("metadata.json"), json) {
                    self.last_error = Some(format!("metadata.json write: {e}"));
                }
            }
            Err(e) => self.last_error = Some(format!("metadata.json serialize: {e}")),
        }
    }

    /// Flush all files if the flush interval elapsed.
    pub fn flush_if_due(&mut self, now: Instant) {
        let due = match &self.session {
            Some(s) => s.dirty && now.duration_since(s.last_flush) >= FLUSH_INTERVAL,
            None => false,
        };
        if due {
            self.force_flush();
            if let Some(s) = &mut self.session {
                s.last_flush = now;
            }
        }
    }

    /// Flush all files now (called on status transitions and commands).
    pub fn force_flush(&mut self) {
        let mut err = None;
        if let Some(s) = &mut self.session {
            for (name, r) in [
                ("raw.cobs", s.raw.flush()),
                ("samples.csv", s.samples.flush()),
                ("events.csv", s.events.flush()),
            ] {
                if let Err(e) = r {
                    err = Some(format!("{name} flush: {e}"));
                }
            }
            s.dirty = false;
        }
        if err.is_some() {
            self.last_error = err;
        }
    }

    /// Close out the session: final event row, final counters into
    /// metadata.json, flush everything.
    pub fn finalize(&mut self, host_unix_ns: i64, stats: &StatsSnapshot) {
        if self.session.is_none() {
            return;
        }
        self.log_event(host_unix_ns, "session_end", "");
        let dir = if let Some(s) = &mut self.session {
            s.meta.session_end_unix_ns = Some(host_unix_ns);
            s.meta.session_end_iso = Some(chrono::Local::now().to_rfc3339());
            s.meta.final_counters = Some(CountersMeta {
                bytes_rx: stats.bytes_rx,
                bytes_tx: stats.bytes_tx,
                frames_ok: stats.frames_ok,
                samples_ok: stats.samples_ok,
                crc_errors: stats.crc_errors,
                framing_errors: stats.framing_errors,
                invalid_samples: stats.invalid_samples,
                seq_gaps: stats.seq_gaps,
                seq_missing: stats.seq_missing,
                ui_events_dropped: stats.ui_events_dropped,
                sample_rows_written: s.sample_rows,
                event_rows_written: s.event_rows,
            });
            Some(s.dir.clone())
        } else {
            None
        };
        if let Some(dir) = dir {
            self.write_metadata(&dir);
        }
        self.force_flush();
    }
}

fn samples_csv_header() -> String {
    let mut h = String::from(
        "host_receive_unix_ns,sequence,monotonic_us,rtc_unix_ms,run_id,flags,\
         heater_active,rtc_valid,pgood,excitation_active,adc_error,\
         state,state_name,sample_valid,excitation_raw,excitation_valid,v24_raw,\
         channel_count",
    );
    for c in &CHANNELS {
        let id = c.id.to_lowercase();
        use std::fmt::Write as _;
        let _ = write!(h, ",{id}_raw,{id}_res_ohm,{id}_temp_c,{id}_valid");
    }
    h.push('\n');
    h
}

/// Minimal CSV field escaping for the free-text `detail` column.
fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_rules() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn header_has_expected_column_count() {
        let header = samples_csv_header();
        // 18 fixed columns + 4 per channel (probe 1 T1..T6, probe 2 T1..T6).
        assert_eq!(header.trim_end().split(',').count(), 18 + 12 * 4);
        assert!(header.contains(",channel_count,t1_raw,"));
        assert!(header.contains(",t6_valid,p2t1_raw,"));
        assert!(header.trim_end().ends_with(",p2t6_valid"));
    }
}
