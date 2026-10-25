//! Pulse-window binning and the RAM window store
//! (`docs/field-protocol.md` §1, `WindowInfo` / `WindowChunk`).
//!
//! Frames are binned on the monotonic clock as they arrive: bin `k` covers
//! `[start + k·bin_ms, start + (k+1)·bin_ms)`. Each channel accumulates the
//! sum and count of its valid raw readings (anything but [`INVALID_RAW`]).
//! When a bin closes it is encoded straight into the slot's byte buffer with
//! [`encode_bin_value`], so a window costs exactly its wire size in RAM.
//!
//! `base_raw[c]` is the rounded mean of channel `c` in bin 0. If a channel
//! has no valid reading in bin 0, its base comes from the first bin that has
//! one; every earlier bin of that channel is the missing sentinel anyway, so
//! decoding is unaffected. A channel with no valid reading in the whole
//! window has base 0 and every bin missing.
//!
//! The store keeps `SLOTS` windows of at most `BYTES / 26` bins. Beginning a
//! window when every slot is taken overwrites the oldest undelivered window
//! (`windows_dropped` counts it). A window that would need more bins than a
//! slot holds is cut at the slot size and flagged `TRUNCATED`.
//!
//! A window begun with [`WindowParams::sfk1`] is not binned: its frames go
//! to the attached [`Sfk1Window`] ([`WindowStore::attach_sfk1`]), and
//! finishing it writes one SFK1 record per probe into the slot instead
//! (`WINDOW_FLAG_SFK1`). Its start moves to the records' frame 0. Without an
//! attached encoder such a window is binned as usual.
//!
//! The struct is all-zero when empty, so a `static` of it lives in `.bss`.

use crate::sfk1win::{Sfk1Window, PROBES};
use sap_flux_protocol::crc::Crc32;
use sap_flux_protocol::field::{encode_bin_value, mean_raw_rounded};
use sap_flux_protocol::{
    ProbeKind, Sfk1Knots, Sfk1Preset, Sfk1Prior, WindowChunk, WindowInfo, WINDOW_CHANNELS,
    WINDOW_CHUNK_DATA_LEN,
    WINDOW_FLAG_ABORTED, WINDOW_FLAG_ADC_ERRORS, WINDOW_FLAG_SFK1, WINDOW_FLAG_TIME_INVALID,
    WINDOW_FLAG_TRUNCATED,
};

pub const CHANNELS: usize = WINDOW_CHANNELS;
pub const BIN_BYTES: usize = CHANNELS * 2;
/// Raw value of a failed ADC read; never binned.
pub const INVALID_RAW: i16 = i16::MIN;
/// Remembered acknowledged run ids, so a retried `AckWindow` is still OK.
const ACKED_MEMORY: usize = 8;

/// Parameters fixed at window start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowParams {
    pub run_id: u32,
    pub start_us: u64,
    pub start_rtc_ms: i64,
    pub bin_ms: u16,
    pub sample_hz: u16,
    pub pulse_duration_ms: u16,
    pub time_invalid: bool,
    /// Encode the window as SFK1 records instead of bins, unless
    /// [`Sfk1Params::OFF`].
    pub sfk1: Sfk1Params,
}

/// What an SFK1 window needs to know at its start. A window is SFK1 when a
/// probe has a kind.
///
/// Not an `Option`: its `None` would sit in a `ProbeKind` niche as a nonzero
/// byte and move the whole store out of `.bss`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sfk1Params {
    /// Probe on each port; with its preset it picks the profile.
    pub probes: [ProbeKind; PROBES],
    /// SFK1 preset of each port.
    pub presets: [Sfk1Preset; PROBES],
    /// Baseline length: the heater fires this long after the first frame.
    pub baseline_s: u16,
}

impl Sfk1Params {
    /// A binned window.
    pub const OFF: Self = Self {
        probes: [ProbeKind::None; PROBES],
        presets: [Sfk1Preset { knots: Sfk1Knots::K12, prior: Sfk1Prior::Slow }; PROBES],
        baseline_s: 0,
    };

    /// Whether any probe is encoded.
    pub fn is_on(&self) -> bool {
        self.probes.iter().any(|&k| k != ProbeKind::None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SlotState {
    Free = 0,
    Recording = 1,
    Pending = 2,
}

#[derive(Clone, Copy, Debug)]
struct SlotMeta {
    state: SlotState,
    order: u32,
    info: WindowInfo,
}

const ZERO_INFO: WindowInfo = WindowInfo {
    run_id: 0,
    window_flags: 0,
    channels: 0,
    bin_ms_div10: 0,
    n_bins: 0,
    chunk_count: 0,
    start_monotonic_us: 0,
    start_rtc_unix_ms: 0,
    end_monotonic_us: 0,
    end_rtc_unix_ms: 0,
    heater_on_offset_ms: 0,
    pulse_duration_ms: 0,
    sample_hz: 0,
    v24_mean_raw: 0,
    frames_total: 0,
    frames_missing: 0,
    data_crc32: 0,
    base_raw: [0; CHANNELS],
};

const FREE_SLOT: SlotMeta = SlotMeta {
    state: SlotState::Free,
    order: 0,
    info: ZERO_INFO,
};

#[derive(Clone, Copy, Debug)]
struct Recorder {
    active: bool,
    slot: usize,
    params: WindowParams,
    bin_us: u64,
    /// Bin currently accumulating; all lower bins are written.
    cur_bin: usize,
    sums: [i32; CHANNELS],
    counts: [u16; CHANNELS],
    base: [i16; CHANNELS],
    base_set: u16,
    frames_good: u32,
    v24_sum: i64,
    v24_count: u32,
    heater_on: bool,
    heater_on_us: u64,
    flags: u16,
    /// Frames go to the attached SFK1 encoder, not to bins.
    sfk1: bool,
}

const IDLE_RECORDER: Recorder = Recorder {
    active: false,
    slot: 0,
    params: WindowParams {
        run_id: 0,
        start_us: 0,
        start_rtc_ms: 0,
        bin_ms: 0,
        sample_hz: 0,
        pulse_duration_ms: 0,
        time_invalid: false,
        sfk1: Sfk1Params::OFF,
    },
    bin_us: 0,
    cur_bin: 0,
    sums: [0; CHANNELS],
    counts: [0; CHANNELS],
    base: [0; CHANNELS],
    base_set: 0,
    frames_good: 0,
    v24_sum: 0,
    v24_count: 0,
    heater_on: false,
    heater_on_us: 0,
    flags: 0,
    sfk1: false,
};

pub struct WindowStore<const SLOTS: usize, const BYTES: usize> {
    data: [[u8; BYTES]; SLOTS],
    meta: [SlotMeta; SLOTS],
    rec: Recorder,
    order: u32,
    dropped: u16,
    last_run_id: u32,
    acked: [u32; ACKED_MEMORY],
    acked_next: usize,
    sfk1: Option<&'static mut Sfk1Window>,
}

impl<const SLOTS: usize, const BYTES: usize> Default for WindowStore<SLOTS, BYTES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SLOTS: usize, const BYTES: usize> WindowStore<SLOTS, BYTES> {
    pub const CAPACITY_BINS: usize = BYTES / BIN_BYTES;

    pub const fn new() -> Self {
        Self {
            data: [[0; BYTES]; SLOTS],
            meta: [FREE_SLOT; SLOTS],
            rec: IDLE_RECORDER,
            order: 0,
            dropped: 0,
            last_run_id: 0,
            acked: [0; ACKED_MEMORY],
            acked_next: 0,
            sfk1: None,
        }
    }

    /// Give the store the encoder for [`WindowParams::sfk1`] windows.
    pub fn attach_sfk1(&mut self, encoder: &'static mut Sfk1Window) {
        self.sfk1 = Some(encoder);
    }

    /// Run id of the window being recorded.
    pub fn recording(&self) -> Option<u32> {
        self.rec.active.then_some(self.rec.params.run_id)
    }

    pub fn windows_pending(&self) -> usize {
        self.meta
            .iter()
            .filter(|m| m.state == SlotState::Pending)
            .count()
    }

    pub fn windows_dropped(&self) -> u16 {
        self.dropped
    }

    /// Run id of the most recently completed window (0 if none).
    pub fn last_run_id(&self) -> u32 {
        self.last_run_id
    }

    pub fn oldest_pending_run_id(&self) -> u32 {
        self.oldest_pending()
            .map_or(0, |i| self.meta[i].info.run_id)
    }

    fn oldest_pending(&self) -> Option<usize> {
        (0..SLOTS)
            .filter(|&i| self.meta[i].state == SlotState::Pending)
            .min_by_key(|&i| self.meta[i].order)
    }

    fn find_pending(&self, run_id: u32) -> Option<usize> {
        if run_id == sap_flux_protocol::WINDOW_OLDEST {
            return self.oldest_pending();
        }
        (0..SLOTS).find(|&i| {
            self.meta[i].state == SlotState::Pending && self.meta[i].info.run_id == run_id
        })
    }

    /// Start recording. A window still recording is finished first (flagged
    /// `ABORTED`, ending at the new start).
    pub fn begin(&mut self, params: WindowParams) {
        if self.rec.active {
            self.rec.flags |= WINDOW_FLAG_ABORTED;
            self.finish(params.start_us, params.start_rtc_ms);
        }
        if SLOTS == 0 || Self::CAPACITY_BINS == 0 {
            return;
        }
        let slot = match (0..SLOTS).find(|&i| self.meta[i].state == SlotState::Free) {
            Some(i) => i,
            None => {
                let i = self.oldest_pending().unwrap_or(0);
                self.dropped = self.dropped.saturating_add(1);
                i
            }
        };
        self.meta[slot] = SlotMeta {
            state: SlotState::Recording,
            order: 0,
            info: ZERO_INFO,
        };
        let sfk1 = match self.sfk1.as_deref_mut() {
            Some(enc) if params.sfk1.is_on() => {
                enc.begin(params.sfk1.probes, params.sfk1.presets, params.sfk1.baseline_s);
                true
            }
            _ => false,
        };
        self.rec = Recorder {
            active: true,
            slot,
            params,
            bin_us: params.bin_ms.max(1) as u64 * 1000,
            flags: if params.time_invalid {
                WINDOW_FLAG_TIME_INVALID
            } else {
                0
            },
            sfk1,
            ..IDLE_RECORDER
        };
    }

    /// Add `WINDOW_FLAG_*` bits to the recording window.
    pub fn add_flags(&mut self, flags: u16) {
        if self.rec.active {
            self.rec.flags |= flags;
        }
    }

    /// Record the heater-on time (first call wins).
    pub fn note_heater_on(&mut self, t_us: u64) {
        if self.rec.active && !self.rec.heater_on {
            self.rec.heater_on = true;
            self.rec.heater_on_us = t_us;
        }
    }

    /// Bin one frame: 12 thermistors then excitation, raw counts, with
    /// [`INVALID_RAW`] for failed reads.
    pub fn push(&mut self, t_us: u64, values: &[i16; CHANNELS], v24_raw: i16) {
        if !self.rec.active || t_us < self.rec.params.start_us {
            return;
        }
        if self.rec.sfk1 {
            if let Some(enc) = self.sfk1.as_deref_mut() {
                enc.push(t_us, values);
            }
            self.count_frame(values, v24_raw);
            return;
        }
        let bin = ((t_us - self.rec.params.start_us) / self.rec.bin_us) as usize;
        while self.rec.cur_bin < bin && self.rec.cur_bin < Self::CAPACITY_BINS {
            self.flush_bin();
        }
        if bin >= Self::CAPACITY_BINS {
            self.rec.flags |= WINDOW_FLAG_TRUNCATED;
            return;
        }
        for (c, &v) in values.iter().enumerate() {
            if v != INVALID_RAW {
                self.rec.sums[c] += v as i32;
                self.rec.counts[c] += 1;
            }
        }
        self.count_frame(values, v24_raw);
    }

    /// Frame statistics every window keeps: good frames, ADC errors, 24 V.
    fn count_frame(&mut self, values: &[i16; CHANNELS], v24_raw: i16) {
        if !values.contains(&INVALID_RAW) {
            self.rec.frames_good += 1;
        } else {
            self.rec.flags |= WINDOW_FLAG_ADC_ERRORS;
        }
        self.rec.v24_sum += v24_raw as i64;
        self.rec.v24_count += 1;
    }

    fn flush_bin(&mut self) {
        let rec = &mut self.rec;
        let k = rec.cur_bin;
        if k >= Self::CAPACITY_BINS {
            return;
        }
        let out = &mut self.data[rec.slot][k * BIN_BYTES..(k + 1) * BIN_BYTES];
        for c in 0..CHANNELS {
            let count = rec.counts[c] as u32;
            if count > 0 && rec.base_set & (1 << c) == 0 {
                rec.base[c] = mean_raw_rounded(rec.sums[c], count).unwrap_or(0);
                rec.base_set |= 1 << c;
            }
            let v = encode_bin_value(rec.sums[c], count, rec.base[c]);
            out[c * 2..c * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        rec.sums = [0; CHANNELS];
        rec.counts = [0; CHANNELS];
        rec.cur_bin += 1;
    }

    /// Close the recording window at `end_us` and make it pending.
    pub fn finish(&mut self, end_us: u64, end_rtc_ms: i64) {
        if !self.rec.active {
            return;
        }
        let begun_us = self.rec.params.start_us;
        // An SFK1 window starts at its records' frame 0.
        let start = match self.sfk1.as_deref() {
            Some(enc) if self.rec.sfk1 => enc.record_start_us().unwrap_or(begun_us),
            _ => begun_us,
        };
        let end_us = end_us.max(start);
        let span = end_us - start;
        let (n_bins, data_len, stored_span) = if self.rec.sfk1 {
            let slot = self.rec.slot;
            let written = self
                .sfk1
                .as_deref_mut()
                .map(|enc| enc.finish(&mut self.data[slot]));
            let (len, truncated) = written.map_or((0, false), |w| (w.len, w.truncated));
            if truncated {
                self.rec.flags |= WINDOW_FLAG_TRUNCATED;
            }
            self.rec.flags |= WINDOW_FLAG_SFK1;
            // n_bins carries the byte length; a slot never holds 64 kB
            let len = len.min(u16::MAX as usize);
            (len, len, span)
        } else {
            let partial = self.rec.counts.iter().any(|&n| n > 0) as usize;
            let mut n_bins = (span.div_ceil(self.rec.bin_us) as usize)
                .max(self.rec.cur_bin + partial)
                .max(1);
            if n_bins > Self::CAPACITY_BINS {
                n_bins = Self::CAPACITY_BINS;
                self.rec.flags |= WINDOW_FLAG_TRUNCATED;
            }
            while self.rec.cur_bin < n_bins {
                self.flush_bin();
            }
            (
                n_bins,
                n_bins * BIN_BYTES,
                span.min(n_bins as u64 * self.rec.bin_us),
            )
        };

        let rec = self.rec;
        if !rec.heater_on {
            self.rec.flags |= WINDOW_FLAG_ABORTED;
        }
        let mut crc = Crc32::new();
        crc.update(&self.data[rec.slot][..data_len]);
        let start_rtc_ms = if rec.params.start_rtc_ms >= 0 {
            rec.params.start_rtc_ms + ((start - begun_us) / 1000) as i64
        } else {
            rec.params.start_rtc_ms
        };

        let frames_total = (stored_span * rec.params.sample_hz as u64 + 500_000) / 1_000_000;
        let frames_total = frames_total.min(u16::MAX as u64) as u32;
        let v24_mean = if rec.v24_count > 0 {
            let m = (rec.v24_sum + rec.v24_count as i64 / 2) / rec.v24_count as i64;
            m.clamp(i16::MIN as i64, i16::MAX as i64) as i16
        } else {
            0
        };

        self.order = self.order.wrapping_add(1);
        self.meta[rec.slot] = SlotMeta {
            state: SlotState::Pending,
            order: self.order,
            info: WindowInfo {
                run_id: rec.params.run_id,
                window_flags: self.rec.flags,
                channels: CHANNELS as u8,
                bin_ms_div10: if rec.sfk1 {
                    0
                } else {
                    (rec.params.bin_ms / 10) as u8
                },
                n_bins: n_bins as u16,
                chunk_count: data_len.div_ceil(WINDOW_CHUNK_DATA_LEN) as u16,
                start_monotonic_us: start,
                start_rtc_unix_ms: start_rtc_ms,
                end_monotonic_us: end_us,
                end_rtc_unix_ms: end_rtc_ms,
                heater_on_offset_ms: if rec.heater_on {
                    (rec.heater_on_us.saturating_sub(start) / 1000).min(u32::MAX as u64) as u32
                } else {
                    0
                },
                pulse_duration_ms: if rec.heater_on {
                    rec.params.pulse_duration_ms
                } else {
                    0
                },
                sample_hz: rec.params.sample_hz,
                v24_mean_raw: v24_mean,
                frames_total: frames_total as u16,
                frames_missing: frames_total.saturating_sub(rec.frames_good) as u16,
                data_crc32: crc.finalize(),
                base_raw: rec.base,
            },
        };
        self.last_run_id = rec.params.run_id;
        self.rec = IDLE_RECORDER;
    }

    /// `WindowInfo` of a pending window (`run_id` 0 = oldest).
    pub fn info(&self, run_id: u32) -> Option<WindowInfo> {
        self.find_pending(run_id).map(|i| self.meta[i].info)
    }

    /// One data chunk of a pending window.
    pub fn chunk(&self, run_id: u32, chunk: u16) -> Option<WindowChunk> {
        let i = self.find_pending(run_id)?;
        let info = &self.meta[i].info;
        let range = sap_flux_protocol::field::chunk_range(chunk, info.data_len())?;
        WindowChunk::new(info.run_id, chunk, &self.data[i][range])
    }

    /// Whether `run_id` is a pending (completed, undelivered) RAM window.
    pub fn has_pending(&self, run_id: u32) -> bool {
        run_id != 0 && self.find_pending(run_id).is_some()
    }

    /// Header and data bytes of a pending window (`run_id` 0 = oldest).
    pub fn pending_window(&self, run_id: u32) -> Option<(&WindowInfo, &[u8])> {
        let i = self.find_pending(run_id)?;
        let info = &self.meta[i].info;
        Some((info, &self.data[i][..info.data_len()]))
    }

    /// Free a pending window that now lives elsewhere (persisted to flash),
    /// without counting it as delivered.
    pub fn release(&mut self, run_id: u32) -> bool {
        if run_id == 0 {
            return false;
        }
        match self.find_pending(run_id) {
            Some(i) => {
                self.meta[i] = FREE_SLOT;
                true
            }
            None => false,
        }
    }

    /// Free a delivered window. True if it was pending or was already
    /// acknowledged recently (a retried `AckWindow`).
    pub fn ack(&mut self, run_id: u32) -> bool {
        if run_id == 0 {
            return false;
        }
        if let Some(i) = self.find_pending(run_id) {
            self.meta[i] = FREE_SLOT;
            self.acked[self.acked_next] = run_id;
            self.acked_next = (self.acked_next + 1) % ACKED_MEMORY;
            return true;
        }
        self.acked.contains(&run_id)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use sap_flux_protocol::field::{read_bin_value, BIN_VALUE_MISSING};
    use sap_flux_protocol::WINDOW_FLAG_HEATER_FAULT;
    use std::vec::Vec;

    type Store = WindowStore<3, { 40 * BIN_BYTES }>;

    fn params(run_id: u32) -> WindowParams {
        WindowParams {
            run_id,
            start_us: 1_000_000,
            start_rtc_ms: 1_792_800_000_000,
            bin_ms: 500,
            sample_hz: 25,
            pulse_duration_ms: 1000,
            time_invalid: false,
            sfk1: Sfk1Params::OFF,
        }
    }

    fn frame(v: i16) -> [i16; CHANNELS] {
        let mut f = [0i16; CHANNELS];
        for (c, x) in f.iter_mut().enumerate() {
            *x = v + 100 * c as i16;
        }
        f
    }

    fn data(store: &Store, run_id: u32) -> Vec<u8> {
        let info = store.info(run_id).unwrap();
        let mut out = Vec::new();
        for k in 0..info.chunk_count {
            out.extend_from_slice(store.chunk(run_id, k).unwrap().bytes());
        }
        assert!(store.chunk(run_id, info.chunk_count).is_none());
        out
    }

    /// 25 Hz frames over `secs` seconds starting at the window start.
    fn record(store: &mut Store, p: WindowParams, secs: u64, value: impl Fn(u64) -> i16) {
        store.begin(p);
        for i in 0..secs * 25 {
            let t = p.start_us + i * 40_000;
            if i == 10 {
                store.note_heater_on(t);
            }
            store.push(t, &frame(value(i)), 2000);
        }
        store.finish(
            p.start_us + secs * 1_000_000,
            p.start_rtc_ms + secs as i64 * 1000,
        );
    }

    /// A 242 s SFK1 window: 60 s baseline, heater on 20 ms after the record
    /// onset frame, frames 7 ms after the window begins.
    fn record_sfk1(store: &mut Store, run_id: u32, probes: [ProbeKind; 2]) {
        let mut p = params(run_id);
        p.sfk1 = Sfk1Params {
            probes,
            presets: Sfk1Params::OFF.presets,
            baseline_s: 60,
        };
        store.begin(p);
        let first = p.start_us + 7_000;
        for i in 0..242 * 25u64 {
            let t = first + i * 40_000;
            if i == 1500 {
                store.note_heater_on(t + 20_000);
            }
            let rel = i as f64 / 25.0 - 60.0;
            let rise = if rel > 0.0 {
                300.0 * (rel / 8.0).min(1.0) * (-rel / 150.0).exp()
            } else {
                0.0
            };
            let mut f = [24_900i16; CHANNELS];
            for (c, x) in f.iter_mut().enumerate().take(12) {
                *x = (11_000.0 + 40.0 * c as f64 + rise) as i16;
            }
            store.push(t, &f, 2000);
        }
        store.finish(first + 242_000_000, p.start_rtc_ms + 242_007);
    }

    #[test]
    fn sfk1_windows_hold_one_record_per_probe() {
        use sap_flux_protocol::field::sfk1_records;
        use sfk1::Profile;
        let mut store = Store::new();
        store.attach_sfk1(std::boxed::Box::leak(std::boxed::Box::default()));
        record_sfk1(&mut store, 5, [ProbeKind::East30Offset, ProbeKind::East30]);
        let info = store.info(5).unwrap();
        assert!(info.is_sfk1() && info.is_consistent());
        assert_eq!(info.window_flags, WINDOW_FLAG_SFK1);
        assert_eq!((info.bin_ms_div10, info.base_raw), (0, [0; CHANNELS]));
        // starts at the first frame, and the heater offset is measured from it
        let p = params(5);
        assert_eq!(info.start_monotonic_us, p.start_us + 7_000);
        assert_eq!(info.start_rtc_unix_ms, p.start_rtc_ms + 7);
        assert_eq!(info.heater_on_offset_ms, 60_020);
        assert_eq!((info.frames_total, info.frames_missing), (6050, 0));
        let bytes = data(&store, 5);
        assert_eq!(bytes.len(), info.n_bins as usize);
        assert!(info.verify_data(&bytes));
        let names: Vec<_> = sfk1_records(&bytes)
            .map(|e| {
                let (_, rec) = e.unwrap();
                let d = sfk1::decode(rec).unwrap();
                assert_eq!((d.n_channels, d.n_bins, d.onset_frame), (6, 12, 1500));
                d.profile
            })
            .collect();
        assert_eq!(names, [sfk1::East30Offset::NAME, sfk1::East30K12::NAME]);
        let (_, d) = store.pending_window(5).unwrap();
        assert_eq!(d, &bytes[..]);
    }

    #[test]
    fn sfk1_windows_without_an_encoder_are_binned() {
        let mut store = Store::new();
        let mut p = params(6);
        p.sfk1 = Sfk1Params {
            probes: [ProbeKind::East30; 2],
            presets: Sfk1Params::OFF.presets,
            baseline_s: 60,
        };
        store.begin(p);
        store.push(p.start_us, &frame(1000), 2000);
        store.finish(p.start_us + 1_000_000, p.start_rtc_ms + 1000);
        let info = store.info(6).unwrap();
        assert!(!info.is_sfk1());
        assert_eq!(info.n_bins, 2);
    }

    #[test]
    fn bins_encode_relative_to_first_bin_mean() {
        let mut store = Store::new();
        // Bin 0 frames alternate 1000/1001; bin 1 is constant 1010.
        record(&mut store, params(7), 1, |i| {
            if i < 13 {
                1000 + (i % 2) as i16
            } else {
                1010
            }
        });
        let info = store.info(7).unwrap();
        assert!(info.is_consistent());
        assert_eq!(info.n_bins, 2);
        assert_eq!(info.bin_ms(), 500);
        // Bin 0 holds frames 0..=12: 7×1000 + 6×1001 = 13006 over 13 frames,
        // mean 1000.46 → base 1000.
        let d = data(&store, 7);
        assert!(info.verify_data(&d));
        assert_eq!(info.base_raw[0], 1000);
        assert_eq!(info.base_raw[12], 2200);
        let v0 = read_bin_value(&d, 0, 0, CHANNELS).unwrap();
        // 16 × (13006/13 − 1000) = 7.38 → 7.
        assert_eq!(v0, 7);
        assert_eq!(read_bin_value(&d, 1, 0, CHANNELS).unwrap(), 160);
        assert_eq!(read_bin_value(&d, 1, 12, CHANNELS).unwrap(), 160);
        assert_eq!(info.heater_on_offset_ms, 400);
        assert_eq!(info.frames_total, 25);
        assert_eq!(info.frames_missing, 0);
        assert_eq!(info.v24_mean_raw, 2000);
        assert_eq!(info.window_flags, 0);
    }

    #[test]
    fn gaps_and_invalid_reads_are_missing() {
        let mut store = Store::new();
        let p = params(9);
        store.begin(p);
        let mut f = frame(500);
        f[3] = INVALID_RAW;
        store.push(p.start_us, &f, 0);
        // Nothing in bin 1; bin 2 complete.
        store.push(p.start_us + 1_000_000, &frame(500), 0);
        store.note_heater_on(p.start_us + 1_000_000);
        store.finish(p.start_us + 1_500_000, -1);
        let info = store.info(9).unwrap();
        let d = data(&store, 9);
        assert_eq!(info.n_bins, 3);
        assert_eq!(read_bin_value(&d, 0, 3, CHANNELS), Some(BIN_VALUE_MISSING));
        assert_eq!(read_bin_value(&d, 0, 4, CHANNELS), Some(0));
        for c in 0..CHANNELS {
            assert_eq!(read_bin_value(&d, 1, c, CHANNELS), Some(BIN_VALUE_MISSING));
        }
        // Channel 3's base comes from bin 2, its first valid reading.
        assert_eq!(info.base_raw[3], 800);
        assert_eq!(read_bin_value(&d, 2, 3, CHANNELS), Some(0));
        assert_ne!(info.window_flags & WINDOW_FLAG_ADC_ERRORS, 0);
        // 1.5 s at 25 Hz = 38 frames expected, 1 complete frame recorded.
        assert_eq!(info.frames_total, 38);
        assert_eq!(info.frames_missing, 37);
    }

    #[test]
    fn no_heater_means_aborted_and_flags_carry() {
        let mut store = Store::new();
        let p = WindowParams {
            time_invalid: true,
            ..params(3)
        };
        store.begin(p);
        store.push(p.start_us, &frame(1), 0);
        store.add_flags(WINDOW_FLAG_HEATER_FAULT);
        store.finish(p.start_us + 100_000, -1);
        let info = store.info(3).unwrap();
        assert_eq!(
            info.window_flags,
            WINDOW_FLAG_ABORTED | WINDOW_FLAG_TIME_INVALID | WINDOW_FLAG_HEATER_FAULT
        );
        assert_eq!(info.pulse_duration_ms, 0);
        assert_eq!(info.n_bins, 1);
    }

    #[test]
    fn long_window_is_truncated_at_slot_size() {
        let mut store = Store::new();
        record(&mut store, params(5), 30, |_| 1234);
        let info = store.info(5).unwrap();
        assert_eq!(info.n_bins as usize, Store::CAPACITY_BINS);
        assert_ne!(info.window_flags & WINDOW_FLAG_TRUNCATED, 0);
        assert!(info.is_consistent());
        assert!(info.verify_data(&data(&store, 5)));
        assert_eq!(info.frames_total, 500);
    }

    #[test]
    fn oldest_is_overwritten_and_ack_frees() {
        let mut store = Store::new();
        for run in 1..=3 {
            record(&mut store, params(run), 1, |_| 10);
        }
        assert_eq!(store.windows_pending(), 3);
        assert_eq!(store.oldest_pending_run_id(), 1);
        assert_eq!(store.info(0).unwrap().run_id, 1);
        record(&mut store, params(4), 1, |_| 10);
        assert_eq!(store.windows_dropped(), 1);
        assert!(store.info(1).is_none());
        assert_eq!(store.oldest_pending_run_id(), 2);
        assert_eq!(store.last_run_id(), 4);

        assert!(store.ack(2));
        assert!(store.ack(2), "retried ack stays OK");
        assert!(!store.ack(99));
        assert!(!store.ack(0));
        assert_eq!(store.windows_pending(), 2);
        assert_eq!(store.oldest_pending_run_id(), 3);
        // The freed slot is reused without dropping anything.
        record(&mut store, params(5), 1, |_| 10);
        assert_eq!(store.windows_dropped(), 1);
        assert_eq!(store.windows_pending(), 3);
    }

    #[test]
    fn recording_window_is_not_served() {
        let mut store = Store::new();
        store.begin(params(8));
        assert_eq!(store.recording(), Some(8));
        assert!(store.info(8).is_none());
        assert!(store.info(0).is_none());
        // A new begin closes the old one as aborted.
        store.begin(WindowParams {
            start_us: 2_000_000,
            ..params(9)
        });
        let info = store.info(8).unwrap();
        assert_ne!(info.window_flags & WINDOW_FLAG_ABORTED, 0);
        assert_eq!(store.recording(), Some(9));
    }

    #[test]
    fn default_schedule_window_fits_production_slot() {
        // Production: 4 slots × 1000 bins.
        type Prod = WindowStore<1, { 1000 * BIN_BYTES }>;
        let bins = sap_flux_protocol::Schedule::FACTORY_DEFAULT.window_bins() as usize;
        assert!(bins <= Prod::CAPACITY_BINS);
        assert_eq!(bins, 482);
    }
}
