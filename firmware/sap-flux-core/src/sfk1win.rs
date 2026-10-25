//! SFK1 encoding of a field window: one record per probe
//! (`docs/field-protocol.md` §1, SFK1 window data).
//!
//! Each probe port with a [`ProbeKind`] gets its own 6-channel encoder with
//! the profile picked by its kind (needle spacing) and [`Sfk1Preset`] (flow
//! range and training prior). Both see the same frames and the same
//! excitation channel. Only the presets in [`LINKED`] are built into the
//! firmware; [`supports`] says whether a schedule can use one.
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
use sap_flux_protocol::{
    ProbeKind, Schedule, Sfk1Knots, Sfk1Preset, Sfk1Prior,
    WINDOW_EXCITATION_CHANNEL,
};
use sfk1::{East30K12, EncodeError, Encoder, Profile};

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

/// Defines the linked presets: one encoder variant each, [`LINKED`],
/// [`supports`] and the constructor. Each code that is not here costs
/// nothing; each knot count here costs about 4 kB of encoder code, and each
/// preset its tables (1.0 / 1.6 / 2.3 / 3.1 kB at 12 / 16 / 20 / 24 knots).
macro_rules! linked {
    ($($profile:ident: $kind:ident $knots:ident $prior:ident,)*) => {
        /// One port's encoder.
        #[allow(clippy::large_enum_variant)] // never moved once the window starts
        enum ProbeEncoder {
            Off,
            $($profile(Encoder<PROBE_CHANNELS, sfk1::$profile>),)*
        }

        /// The (probe kind, preset) pairs this firmware encodes, with the
        /// `sfk1` profile of each.
        pub const LINKED: &[(ProbeKind, Sfk1Preset, &str)] = &[
            $((
                ProbeKind::$kind,
                Sfk1Preset { knots: Sfk1Knots::$knots, prior: Sfk1Prior::$prior },
                <sfk1::$profile as Profile>::NAME,
            ),)*
        ];

        const _: () = {
            $(
                assert!(<sfk1::$profile as Profile>::FRAME_PERIOD_US as u64 == FRAME_PERIOD_US);
                assert!(<sfk1::$profile as Profile>::BIN_FRAMES as u32 == BIN_FRAMES);
                assert!(<sfk1::$profile as Profile>::POST_FRAMES == East30K12::POST_FRAMES);
            )*
            assert!(BIN_FRAMES as u64 * FRAME_PERIOD_US == BIN_S as u64 * 1_000_000);
        };

        impl ProbeEncoder {
            fn new(kind: ProbeKind, preset: Sfk1Preset, n_bins: u8) -> Self {
                let made = match (kind, preset.knots, preset.prior) {
                    $((ProbeKind::$kind, Sfk1Knots::$knots, Sfk1Prior::$prior) => {
                        Encoder::new(&LIVE, n_bins).map(Self::$profile)
                    })*
                    _ => return Self::Off,
                };
                // 2..=12 bins and a 1-byte mask of 6 channels always construct
                made.unwrap_or(Self::Off)
            }

            fn push(&mut self, frame: Option<(i16, &[i16; PROBE_CHANNELS])>) {
                // WindowFull past the record's end: those frames are not encoded
                let _ = match self {
                    Self::Off => Ok(()),
                    $(Self::$profile(e) => match frame {
                        Some((exc, therm)) => e.push(exc, therm),
                        None => e.push_missing(),
                    },)*
                };
            }

            fn is_complete(&self) -> bool {
                match self {
                    Self::Off => true,
                    $(Self::$profile(e) => e.is_complete(),)*
                }
            }

            fn reached_onset(&self) -> bool {
                match self {
                    Self::Off => false,
                    $(Self::$profile(e) => e.frames_pushed() > e.onset_frame(),)*
                }
            }

            fn pad(&mut self) {
                match self {
                    Self::Off => {}
                    $(Self::$profile(e) => while !e.is_complete() && e.push_missing().is_ok() {},)*
                }
            }

            fn finish(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
                match self {
                    Self::Off => Ok(0),
                    $(Self::$profile(e) => e.finish(out).map(|f| f.len),)*
                }
            }
        }
    };
}

// Each preset is monomorphized code plus its tables: about 2.4 kB for another
// preset at a linked knot count, 7.7 kB for the first at a new one. K20 Mixed
// for the offset probe would leave 4.5 kB of flash; drop a preset here before
// trimming anything else when flash runs short.
linked! {
    East30K12: East30 K12 Slow,
    East30Offset: East30Offset K12 Slow,
    East30K20Mixed: East30 K20 Mixed,
}

/// Whether this firmware can encode `kind` with `preset`. A port without a
/// kind takes any preset.
pub fn supports(kind: ProbeKind, preset: Sfk1Preset) -> bool {
    kind == ProbeKind::None || LINKED.iter().any(|&(k, p, _)| (k, p) == (kind, preset))
}

/// Whether this firmware can run `schedule`: a binned schedule always, an
/// SFK1 one when every port's (kind, preset) is [linked](LINKED). Check it
/// with [`Schedule::validate`] before accepting a schedule.
pub fn schedule_supported(schedule: &Schedule) -> bool {
    !schedule.sfk1() || (0..PROBES).all(|p| supports(schedule.probe(p), schedule.preset(p)))
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
    /// A port whose preset is not [linked](LINKED) gets no record.
    pub fn begin(
        &mut self,
        kinds: [ProbeKind; PROBES],
        presets: [Sfk1Preset; PROBES],
        baseline_s: u16,
    ) {
        let n_bins = Self::baseline_bins(baseline_s);
        let baseline_frames = baseline_s as u32 * 1_000_000 / FRAME_PERIOD_US as u32;
        for ((probe, &kind), &preset) in self.probes.iter_mut().zip(&kinds).zip(&presets) {
            *probe = ProbeEncoder::new(kind, preset, n_bins);
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
                if probe.is_complete() {
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
    use sap_flux_protocol::SCHEDULE_FLAG_SFK1;
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

    const K12: [Sfk1Preset; 2] = [Sfk1Preset { knots: Sfk1Knots::K12, prior: Sfk1Prior::Slow }; 2];

    fn run(
        kinds: [ProbeKind; 2],
        baseline_s: u16,
        secs: u64,
        drop: impl Fn(u64) -> bool,
    ) -> (vec::Vec<u8>, u64) {
        run_presets(kinds, K12, baseline_s, secs, drop)
    }

    fn run_presets(
        kinds: [ProbeKind; 2],
        presets: [Sfk1Preset; 2],
        baseline_s: u16,
        secs: u64,
        drop: impl Fn(u64) -> bool,
    ) -> (vec::Vec<u8>, u64) {
        let mut w = Sfk1Window::new();
        w.begin(kinds, presets, baseline_s);
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
            assert_eq!(dec.profile, [East30K12::NAME, sfk1::East30Offset::NAME][p]);
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
        w.begin([ProbeKind::East30, ProbeKind::East30], K12, 60);
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

    #[test]
    fn every_linked_preset_records_and_others_are_refused() {
        for &(kind, preset, name) in LINKED {
            assert!(supports(kind, preset));
            let (data, _) = run_presets([kind, ProbeKind::None], [preset, K12[0]], 60, 242, |_| false);
            let entries: vec::Vec<_> = sfk1_records(&data).map(Result::unwrap).collect();
            assert_eq!(entries.len(), 1);
            let dec = sfk1::decode(entries[0].1).unwrap();
            assert_eq!(dec.profile, name);
            let times = dec.times();
            for (c, ch) in dec.channels.iter().enumerate() {
                let err = ch
                    .samples
                    .iter()
                    .zip(&times)
                    .map(|(&v, &t)| (v - frame(t, 400.0)[c] as f64).abs())
                    .fold(0.0, f64::max);
                assert!(err < 3.0, "{name} ch {c}: max error {err} counts");
            }
        }
        assert!(supports(ProbeKind::None, Sfk1Preset { knots: Sfk1Knots::K24, prior: Sfk1Prior::Fast }));
        // a preset the firmware does not link gives no record, not a wrong one
        let missing = [Sfk1Knots::K12, Sfk1Knots::K16, Sfk1Knots::K20, Sfk1Knots::K24]
            .into_iter()
            .flat_map(|knots| {
                [Sfk1Prior::Slow, Sfk1Prior::Mixed, Sfk1Prior::Fast]
                    .map(|prior| Sfk1Preset { knots, prior })
            })
            .find(|&p| !supports(ProbeKind::East30, p));
        if let Some(p) = missing {
            let sched = Schedule { flags: SCHEDULE_FLAG_SFK1, ..Schedule::FACTORY_DEFAULT }
                .with_probe(0, ProbeKind::East30);
            assert!(schedule_supported(&sched));
            assert!(!schedule_supported(&sched.with_preset(0, p)));
            assert!(schedule_supported(&Schedule { flags: 0, ..sched.with_preset(0, p) }));
            let (data, _) = run_presets([ProbeKind::East30, ProbeKind::None], [p, K12[0]], 60, 242, |_| false);
            assert_eq!(data, [1, 0, 0]);
        }
    }
}
