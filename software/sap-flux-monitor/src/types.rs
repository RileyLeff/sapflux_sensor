//! Types shared between the serial worker, logger, and UI.

use crate::channels::{CHANNEL_COUNT, PROBE_CHANNELS};
use crate::thermo::ChannelReading;
use sap_flux_protocol::{
    BoardState, FieldStatus, Sample, Sample12, FIELD_STATUS_ENABLED, FIELD_STATUS_FLASH_OK,
    FIELD_STATUS_HEATER_LOCKOUT, FIELD_STATUS_LEASE_VALID, FIELD_STATUS_REQUIRE_LEASE,
    FIELD_STATUS_RTC_VALID, FLAG_ADC_ERROR,
};

/// One ADC frame from either a v1 `Sample` (6 thermistors, probe 1 only)
/// or a field `Sample12` (12 thermistors, both probes), normalized to the
/// 12-channel layout. Field names match the protocol structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleData {
    /// Microseconds since boot (monotonic).
    pub monotonic_us: u64,
    /// RTC time in Unix milliseconds; `-1` means the RTC is invalid.
    pub rtc_unix_ms: i64,
    /// Run this sample belongs to (0 when idle).
    pub run_id: u32,
    /// `FLAG_*` bits.
    pub flags: u16,
    /// Raw ADC counts on the excitation-sense channel.
    pub excitation_raw: i16,
    /// Probe 1 T1..T6 then probe 2 T1..T6; entries at and past
    /// `channel_count` are zero and meaningless.
    pub thermistor_raw: [i16; CHANNEL_COUNT],
    /// Number of thermistor channels the frame actually carried (6 or 12).
    pub channel_count: usize,
    /// Raw ADC counts on the 24 V rail monitor.
    pub v24_raw: i16,
    /// Instrument state when the sample was taken.
    pub state: BoardState,
}

impl From<Sample> for SampleData {
    fn from(s: Sample) -> Self {
        let mut thermistor_raw = [0i16; CHANNEL_COUNT];
        thermistor_raw[..PROBE_CHANNELS].copy_from_slice(&s.thermistor_raw);
        Self {
            monotonic_us: s.monotonic_us,
            rtc_unix_ms: s.rtc_unix_ms,
            run_id: s.run_id,
            flags: s.flags,
            excitation_raw: s.excitation_raw,
            thermistor_raw,
            channel_count: PROBE_CHANNELS,
            v24_raw: s.v24_raw,
            state: s.state,
        }
    }
}

impl From<Sample12> for SampleData {
    fn from(s: Sample12) -> Self {
        Self {
            monotonic_us: s.monotonic_us,
            rtc_unix_ms: s.rtc_unix_ms,
            run_id: s.run_id,
            flags: s.flags,
            excitation_raw: s.excitation_raw,
            thermistor_raw: s.thermistor_raw,
            channel_count: CHANNEL_COUNT,
            v24_raw: s.v24_raw,
            state: s.state,
        }
    }
}

/// A decoded Sample/Sample12 frame plus everything derived from it. Built
/// once in the worker; used for both logging (first) and UI delivery.
#[derive(Debug, Clone)]
pub struct ProcessedSample {
    /// Host receive time, Unix nanoseconds.
    pub host_unix_ns: i64,
    /// Host receive time, seconds since worker start (plot x axis).
    pub t_rel_s: f64,
    /// Frame sequence number.
    pub sequence: u32,
    /// The raw decoded sample (normalized to 12 channels).
    pub sample: SampleData,
    /// Per-channel conversions, probe 1 T1..T6 then probe 2 T1..T6. Only
    /// the first `sample.channel_count` entries are meaningful.
    pub readings: [ChannelReading; CHANNEL_COUNT],
    /// True when the excitation reading was usable.
    pub exc_valid: bool,
    /// True when the frame produced usable data (no ADC error, live
    /// excitation reading).
    pub sample_valid: bool,
}

impl ProcessedSample {
    /// Convert a decoded Sample or Sample12 into a processed one.
    pub fn build(
        sample: impl Into<SampleData>,
        sequence: u32,
        host_unix_ns: i64,
        t_rel_s: f64,
    ) -> Self {
        let sample: SampleData = sample.into();
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

    /// Number of thermistor channels this frame carried (6 or 12).
    pub fn channel_count(&self) -> usize {
        self.sample.channel_count
    }

    /// The readings this frame actually carried.
    pub fn present_readings(&self) -> &[ChannelReading] {
        &self.readings[..self.sample.channel_count]
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

/// Names of the set `FieldStatus.schedule_flags` bits, `|`-joined
/// (`"-"` when none are set).
pub fn field_status_flag_names(flags: u8) -> String {
    const NAMES: [(u8, &str); 6] = [
        (FIELD_STATUS_ENABLED, "enabled"),
        (FIELD_STATUS_REQUIRE_LEASE, "require_lease"),
        (FIELD_STATUS_LEASE_VALID, "lease_valid"),
        (FIELD_STATUS_RTC_VALID, "rtc_valid"),
        (FIELD_STATUS_HEATER_LOCKOUT, "heater_lockout"),
        (FIELD_STATUS_FLASH_OK, "flash_ok"),
    ];
    let set: Vec<&str> = NAMES
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    if set.is_empty() {
        "-".to_string()
    } else {
        set.join("|")
    }
}

/// One-line `key=value` description of a FieldStatus (events.csv detail).
pub fn field_status_detail(fs: &FieldStatus) -> String {
    format!(
        "rtc_ms={} next_pulse_unix_s={} flags={:#04x}({}) pending={} dropped={} \
         oldest_run={} last_run={} calr_ppb={} last_rtc_step_ms={} boot_count={} \
         v24_raw={} flash_used_kib={} schedule_crc={:04x}",
        fs.rtc_unix_ms,
        fs.next_pulse_unix_s,
        fs.schedule_flags,
        field_status_flag_names(fs.schedule_flags),
        fs.windows_pending,
        fs.windows_dropped,
        fs.oldest_pending_run_id,
        fs.last_run_id,
        fs.calr_ppb,
        fs.last_rtc_step_ms,
        fs.boot_count,
        fs.v24_raw,
        fs.flash_used_kib,
        fs.schedule_crc
    )
}
