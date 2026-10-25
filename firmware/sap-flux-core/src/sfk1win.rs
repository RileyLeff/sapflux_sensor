//! SFK1 encoding of a field window: one record per probe
//! (`docs/field-protocol.md` §1, SFK1 window data).
//!
//! Each probe port with a [`ProbeKind`] gets its own 6-channel encoder, with
//! the profile its needle spacing was trained for: classic East30 probes
//! [`East30K12`], offset probes [`East30Offset`]. Both see the same frames
//! and the same excitation channel.
//!
//! Frames are placed on the 40 ms grid by time, not by count: the first
//! frame after [`begin`](Sfk1Window::begin) is window frame 0, and a frame at
//! `t` is window frame `round((t − t₀) / 40 ms)`. Slots with no frame (a
//! stalled loop) become missing frames. A record starts `skip` window frames
//! in, so that its onset frame lands `baseline_s` after window frame 0, with
//! `min(12, baseline_s / 5)` baseline bins: a baseline longer than 60 s or
//! not a multiple of 5 s drops its oldest frames. A frame with a failed read
//! ([`INVALID_RAW`]) on the excitation or on any of a probe's thermistors is
//! a missing frame of that probe's record.

use crate::window::{CHANNELS, INVALID_RAW};
use sap_flux_protocol::field::{write_sfk1_prefix, SFK1_ENTRY_PREFIX_LEN};
use sap_flux_protocol::{ProbeKind, WINDOW_EXCITATION_CHANNEL};
use sfk1::{East30K12, East30Offset, EncodeError, Encoder, Profile};

/// Probe ports on the board.
pub const PROBES: usize = 2;
/// Thermistors per probe: window channels `6·p .. 6·p + 6`.
pub const PROBE_CHANNELS: usize = 6;
/// Frame period of the East30 profiles.
pub const FRAME_PERIOD_US: u64 = East30K12::FRAME_PERIOD_US as u64;

const LIVE: [u8; 1] = [0b0011_1111];
const BIN_FRAMES: u32 = East30K12::BIN_FRAMES as u32;
const BIN_S: u16 = (BIN_FRAMES as u64 * FRAME_PERIOD_US / 1_000_000) as u16;
const MAX_BINS: u16 = 12;

const _: () = {
    assert!(East30K12::FRAME_PERIOD_US == East30Offset::FRAME_PERIOD_US);
    assert!(East30K12::BIN_FRAMES == East30Offset::BIN_FRAMES);
    assert!(East30K12::POST_FRAMES == East30Offset::POST_FRAMES);
    assert!(BIN_FRAMES as u64 * FRAME_PERIOD_US == BIN_S as u64 * 1_000_000);
};

/// One port's encoder.
#[allow(clippy::large_enum_variant)] // never moved once the window starts
enum ProbeEncoder {
    Off,
    East30(Encoder<PROBE_CHANNELS, East30K12>),
    Offset(Encoder<PROBE_CHANNELS, East30Offset>),
}

macro_rules! with_encoder {
    ($probe:expr, $enc:ident => $body:expr, $off:expr) => {
        match $probe {
            ProbeEncoder::Off => $off,
            ProbeEncoder::East30($enc) => $body,
            ProbeEncoder::Offset($enc) => $body,
        }
    };
}

impl ProbeEncoder {
    fn new(kind: ProbeKind, n_bins: u8) -> Self {
        let made = match kind {
            ProbeKind::None => return Self::Off,
            ProbeKind::East30 => Encoder::new(&LIVE, n_bins).map(Self::East30),
            ProbeKind::East30Offset => Encoder::new(&LIVE, n_bins).map(Self::Offset),
        };
        // 2..=12 bins and a 1-byte mask of 6 channels always construct
        made.unwrap_or(Self::Off)
    }

    fn push(&mut self, frame: Option<(i16, &[i16; PROBE_CHANNELS])>) {
        // WindowFull past the record's end: those frames are not encoded
        let _ = with_encoder!(self, e => match frame {
            Some((exc, therm)) => e.push(exc, therm),
            None => e.push_missing(),
        }, Ok(()));
    }

    fn reached_onset(&self) -> bool {
        with_encoder!(self, e => e.frames_pushed() > e.onset_frame(), false)
    }

    fn pad(&mut self) {
        with_encoder!(self, e => while !e.is_complete() && e.push_missing().is_ok() {}, ())
    }

    fn finish(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        with_encoder!(self, e => e.finish(out).map(|f| f.len), Ok(0))
    }
}

/// What [`Sfk1Window::finish`] wrote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sfk1Written {
    /// Data length in bytes.
    pub len: usize,
    /// `out` was too small for some entry; it and later probes are left out.
    pub truncated: bool,
}

/// SFK1 encoders for one window, one per probe port.
///
/// About 1.5 kB per port. It is not all-zero when idle, so keep it out of a
/// zero-initialised static (a `StaticCell`, say) and attach it to the
/// window store.
pub struct Sfk1Window {
    probes: [ProbeEncoder; PROBES],
    kinds: [ProbeKind; PROBES],
    active: bool,
    skip: u32,
    origin_us: Option<u64>,
    /// Record frames handed to the encoders so far.
    next: u32,
}

impl Default for Sfk1Window {
    fn default() -> Self {
        Self::new()
    }
}

impl Sfk1Window {
    pub const fn new() -> Self {
        Self {
            probes: [ProbeEncoder::Off, ProbeEncoder::Off],
            kinds: [ProbeKind::None; PROBES],
            active: false,
            skip: 0,
            origin_us: None,
            next: 0,
        }
    }

    /// Baseline bins a window with `baseline_s` uses: `min(12, baseline_s / 5)`,
    /// at least 2.
    pub fn baseline_bins(baseline_s: u16) -> u8 {
        (baseline_s / BIN_S).clamp(2, MAX_BINS) as u8
    }

    /// Start a window whose heater fires `baseline_s` after its first frame.
    pub fn begin(&mut self, kinds: [ProbeKind; PROBES], baseline_s: u16) {
        let n_bins = Self::baseline_bins(baseline_s);
        let baseline_frames = baseline_s as u32 * 1_000_000 / FRAME_PERIOD_US as u32;
        for (probe, &kind) in self.probes.iter_mut().zip(&kinds) {
            *probe = ProbeEncoder::new(kind, n_bins);
        }
        self.kinds = kinds;
        self.active = true;
        self.skip = baseline_frames.saturating_sub(n_bins as u32 * BIN_FRAMES);
        self.origin_us = None;
        self.next = 0;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Monotonic time of record frame 0, once the first frame has arrived.
    pub fn record_start_us(&self) -> Option<u64> {
        self.origin_us
            .map(|t0| t0 + self.skip as u64 * FRAME_PERIOD_US)
    }

    /// Feed one window frame: 12 thermistors then excitation, raw counts.
    pub fn push(&mut self, t_us: u64, values: &[i16; CHANNELS]) {
        if !self.active {
            return;
        }
        let t0 = *self.origin_us.get_or_insert(t_us);
        let Some(dt) = t_us.checked_sub(t0) else {
            return;
        };
        let slot = (dt + FRAME_PERIOD_US / 2) / FRAME_PERIOD_US;
        let Some(i) = slot.checked_sub(self.skip as u64) else {
            return;
        };
        let i = i.min(u32::MAX as u64) as u32;
        if i < self.next {
            return; // a second frame in one slot
        }
        let gap = i - self.next;
        let exc = values[WINDOW_EXCITATION_CHANNEL];
        for (p, probe) in self.probes.iter_mut().enumerate() {
            if matches!(probe, ProbeEncoder::Off) {
                continue;
            }
            for _ in 0..gap {
                if with_encoder!(probe, e => e.is_complete(), true) {
                    break;
                }
                probe.push(None);
            }
            let therm: &[i16; PROBE_CHANNELS] = values
                [p * PROBE_CHANNELS..(p + 1) * PROBE_CHANNELS]
                .try_into()
                .unwrap_or(&[INVALID_RAW; PROBE_CHANNELS]);
            let valid = exc != INVALID_RAW && !therm.contains(&INVALID_RAW);
            probe.push(valid.then_some((exc, therm)));
        }
        self.next = i + 1;
    }

    /// Close the window and write its entries into `out`. A probe whose
    /// record never reached the heater onset (a run aborted in baseline), or
    /// had no valid baseline frame, gets an empty entry; a record cut short
    /// later is completed with missing frames.
    pub fn finish(&mut self, out: &mut [u8]) -> Sfk1Written {
        let mut len = 0;
        let mut truncated = false;
        for (p, probe) in self.probes.iter_mut().enumerate() {
            if self.kinds[p] == ProbeKind::None {
                continue;
            }
            let Some(rest) = out
                .get_mut(len..)
                .filter(|r| r.len() >= SFK1_ENTRY_PREFIX_LEN)
            else {
                truncated = true;
                break;
            };
            let (prefix, body) = rest.split_at_mut(SFK1_ENTRY_PREFIX_LEN);
            let n = if probe.reached_onset() {
                probe.pad();
                match probe.finish(body) {
                    Ok(n) => n,
                    Err(EncodeError::BufferTooSmall { .. }) => {
                        truncated = true;
                        break;
                    }
                    Err(_) => 0,
                }
            } else {
                0
            };
            let prefix: &mut [u8; SFK1_ENTRY_PREFIX_LEN] =
                prefix.try_into().unwrap_or_else(|_| unreachable!());
            write_sfk1_prefix(prefix, p as u8 + 1, n as u16);
            len += SFK1_ENTRY_PREFIX_LEN + n;
        }
        self.active = false;
        Sfk1Written { len, truncated }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use sap_flux_protocol::field::sfk1_records;
    use std::vec;

    const ONSET_S: u64 = 60;

    /// A thermistor that rises by `amp` counts after onset.
    fn frame(t_s: f64, amp: f64) -> [i16; CHANNELS] {
        let mut f = [0i16; CHANNELS];
        for (c, x) in f.iter_mut().enumerate().take(12) {
            let rise = if t_s > 0.0 {
                amp * (1.0 - (-t_s / 20.0).exp()) * (-t_s / 120.0).exp()
            } else {
                0.0
            };
            *x = (11_000.0 + 50.0 * c as f64 + rise * (1.0 + c as f64 / 12.0)) as i16;
        }
        f[WINDOW_EXCITATION_CHANNEL] = 24_900;
        f
    }

    fn run(
        kinds: [ProbeKind; 2],
        baseline_s: u16,
        secs: u64,
        drop: impl Fn(u64) -> bool,
    ) -> (vec::Vec<u8>, u64) {
        let mut w = Sfk1Window::new();
        w.begin(kinds, baseline_s);
        let t0 = 5_000_017u64;
        for n in 0..secs * 25 {
            if drop(n) {
                continue;
            }
            let t = t0 + n * 40_000 + (n * 7919 % 9_000); // up to 9 ms late
            let rel = (n as f64 / 25.0) - baseline_s as f64;
            w.push(t, &frame(rel, 400.0));
        }
        let mut out = vec![0u8; 4096];
        let done = w.finish(&mut out);
        assert!(!done.truncated);
        out.truncate(done.len);
        (out, w.record_start_us().unwrap() - t0)
    }

    #[test]
    fn records_per_probe_decode_with_their_profiles() {
        let kinds = [ProbeKind::East30, ProbeKind::East30Offset];
        let (data, start) = run(kinds, ONSET_S as u16, 242, |_| false);
        assert_eq!(start, 0);
        let entries: vec::Vec<_> = sfk1_records(&data).map(Result::unwrap).collect();
        assert_eq!(entries.len(), 2);
        std::println!(
            "entry lengths {:?}",
            entries.iter().map(|e| e.1.len()).collect::<vec::Vec<_>>()
        );
        for (p, (probe, rec)) in entries.iter().enumerate() {
            assert_eq!(*probe as usize, p + 1);
            let dec = sfk1::decode(rec).unwrap();
            assert_eq!(dec.n_channels, 6);
            assert_eq!(dec.profile, [East30K12::NAME, East30Offset::NAME][p]);
            assert_eq!(dec.channels.len(), 6);
            let times = dec.times();
            for (c, ch) in dec.channels.iter().enumerate() {
                let want = |t: f64| frame(t, 400.0)[p * 6 + c] as f64;
                let err = ch
                    .samples
                    .iter()
                    .zip(&times)
                    .map(|(&v, &t)| (v - want(t)).abs())
                    .fold(0.0, f64::max);
                assert!(err < 3.0, "probe {} ch {c}: max error {err} counts", p + 1);
            }
        }
    }

    #[test]
    fn absent_probe_long_baseline_and_gaps() {
        // 90 s baseline: 12 bins, the first 30 s dropped. Frames 2000..2010
        // never arrive and probe 1 is not fitted.
        let kinds = [ProbeKind::None, ProbeKind::East30];
        let (data, start) = run(kinds, 90, 272, |n| (2000..2010).contains(&n));
        assert_eq!(start, 30 * 1_000_000);
        let entries: vec::Vec<_> = sfk1_records(&data).map(Result::unwrap).collect();
        assert_eq!(entries.len(), 1);
        let (probe, rec) = entries[0];
        assert_eq!(probe, 2);
        let dec = sfk1::decode(rec).unwrap();
        assert_eq!((dec.n_bins, dec.onset_frame), (12, 1500));
    }

    #[test]
    fn aborted_in_baseline_gives_empty_entries() {
        let kinds = [ProbeKind::East30, ProbeKind::East30];
        let (data, _) = run(kinds, 60, 30, |_| false);
        assert_eq!(data, [1, 0, 0, 2, 0, 0]);
        // a window cut 100 s after onset still encodes
        let (data, _) = run(kinds, 60, 160, |_| false);
        assert!(sfk1_records(&data).all(|e| e.unwrap().1.len() > 50));
    }

    #[test]
    fn invalid_reads_are_missing_frames_of_that_probe_only() {
        let mut w = Sfk1Window::new();
        w.begin([ProbeKind::East30, ProbeKind::East30], 60);
        for n in 0..242 * 25u64 {
            let mut f = frame(n as f64 / 25.0 - 60.0, 400.0);
            if n % 100 == 3 {
                f[7] = INVALID_RAW; // probe 2 T2
            }
            w.push(n * 40_000, &f);
        }
        let mut out = vec![0u8; 4096];
        let len = w.finish(&mut out).len;
        let mut it = sfk1_records(&out[..len]).map(Result::unwrap);
        let (_, p1) = it.next().unwrap();
        let (_, p2) = it.next().unwrap();
        assert_eq!(
            sfk1::Decoder::<East30K12>::new(p1)
                .unwrap()
                .header()
                .n_live(),
            6
        );
        assert!(sfk1::decode(p2).is_ok());
        assert_eq!(Sfk1Window::baseline_bins(10), 2);
        assert_eq!(Sfk1Window::baseline_bins(37), 7);
        assert_eq!(Sfk1Window::baseline_bins(300), 12);
        std::println!("Sfk1Window is {} B", core::mem::size_of::<Sfk1Window>());
    }
}
