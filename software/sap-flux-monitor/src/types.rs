//! Types shared between the serial worker, logger, and UI.

use crate::thermo::ChannelReading;
use sap_flux_protocol::{Sample, FLAG_ADC_ERROR};

/// A decoded Sample frame plus everything derived from it. Built once in
/// the worker; used for both logging (first) and UI delivery.
#[derive(Debug, Clone)]
pub struct ProcessedSample {
    /// Host receive time, Unix nanoseconds.
    pub host_unix_ns: i64,
    /// Host receive time, seconds since worker start (plot x axis).
    pub t_rel_s: f64,
    /// Frame sequence number.
    pub sequence: u32,
    /// The raw decoded sample.
    pub sample: Sample,
    /// Per-channel conversions, T1..T6.
    pub readings: [ChannelReading; 6],
    /// True when the excitation reading was usable.
    pub exc_valid: bool,
    /// True when the frame produced usable data (no ADC error, live
    /// excitation reading).
    pub sample_valid: bool,
}

impl ProcessedSample {
    /// Convert a decoded Sample into a processed one.
    pub fn build(sample: Sample, sequence: u32, host_unix_ns: i64, t_rel_s: f64) -> Self {
        let adc_error = sample.flags & FLAG_ADC_ERROR != 0;
        let readings =
            crate::thermo::convert_sample(sample.excitation_raw, &sample.thermistor_raw, adc_error);
        let exc_valid = !adc_error && sample.excitation_raw >= crate::thermo::EXC_MIN_COUNTS;
        Self {
            host_unix_ns,
            t_rel_s,
            sequence,
            sample,
            readings,
            exc_valid,
            sample_valid: exc_valid,
        }
    }
}

/// Current Unix time in nanoseconds.
pub fn host_unix_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Human name for a board state.
pub fn state_name(state: sap_flux_protocol::BoardState) -> &'static str {
    use sap_flux_protocol::BoardState::*;
    match state {
        SafeIdle => "SafeIdle",
        Baseline => "Baseline",
        Armed => "Armed",
        Pulsing => "Pulsing",
        Recovery => "Recovery",
        Cooldown => "Cooldown",
        Fault => "Fault",
    }
}

/// Format a packed firmware version (`major<<16 | minor<<8 | patch`).
pub fn format_fw_version(v: u32) -> String {
    format!("{}.{}.{}", (v >> 16) & 0xFFFF, (v >> 8) & 0xFF, v & 0xFF)
}
