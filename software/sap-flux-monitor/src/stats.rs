//! Link statistics: byte/frame counters, sequence-gap accounting, and a
//! sliding-window complete-frame sample rate.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Sequence-gap tracker over the sender's u32 frame counter (all message
/// types share one counter on the sender side).
#[derive(Debug, Default)]
pub struct SeqTracker {
    last: Option<u32>,
    /// Number of discontinuities observed.
    pub gaps: u64,
    /// Total frames missing across all gaps.
    pub missing: u64,
    /// Number of apparent counter resets (huge backward/forward jumps,
    /// e.g. a board reboot), which are not counted as missing frames.
    pub resets: u64,
}

/// Jumps of at least this size are treated as a counter reset, not a gap.
const RESET_THRESHOLD: u32 = 1 << 20;

impl SeqTracker {
    /// Record a received sequence number. Returns `Some(missing)` when a
    /// gap was detected, with the number of frames skipped.
    pub fn on_sequence(&mut self, seq: u32) -> Option<u64> {
        let result = match self.last {
            None => None,
            Some(last) => {
                let expected = last.wrapping_add(1);
                if seq == expected {
                    None
                } else {
                    let delta = seq.wrapping_sub(expected);
                    if delta >= RESET_THRESHOLD {
                        self.resets += 1;
                        None
                    } else {
                        self.gaps += 1;
                        self.missing += delta as u64;
                        Some(delta as u64)
                    }
                }
            }
        };
        self.last = Some(seq);
        result
    }

    /// Forget the last sequence (on reconnect).
    pub fn reset_baseline(&mut self) {
        self.last = None;
    }
}

/// Sliding-window event rate estimator.
#[derive(Debug)]
pub struct RateWindow {
    times: VecDeque<Instant>,
    window: Duration,
}

impl RateWindow {
    /// Create a rate window of the given width.
    pub fn new(window: Duration) -> Self {
        Self {
            times: VecDeque::new(),
            window,
        }
    }

    /// Record one event.
    pub fn tick(&mut self, now: Instant) {
        self.times.push_back(now);
        self.prune(now);
    }

    /// Events per second over the window.
    pub fn rate(&mut self, now: Instant) -> f64 {
        self.prune(now);
        self.times.len() as f64 / self.window.as_secs_f64()
    }

    fn prune(&mut self, now: Instant) {
        while let Some(&front) = self.times.front() {
            if now.duration_since(front) > self.window {
                self.times.pop_front();
            } else {
                break;
            }
        }
    }
}

/// Snapshot of all link counters, sent from the worker to the UI.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatsSnapshot {
    /// Raw serial bytes received.
    pub bytes_rx: u64,
    /// Raw serial bytes transmitted.
    pub bytes_tx: u64,
    /// Frames of any type decoded successfully.
    pub frames_ok: u64,
    /// Sample frames decoded successfully.
    pub samples_ok: u64,
    /// Complete-frame sample rate (Hz) over the last 2 s.
    pub sample_rate_hz: f64,
    /// CRC failures.
    pub crc_errors: u64,
    /// All other framing/decoding failures (COBS, length, version, ...).
    pub framing_errors: u64,
    /// Samples whose conversions were unusable (ADC error or dead
    /// excitation).
    pub invalid_samples: u64,
    /// Sequence discontinuities.
    pub seq_gaps: u64,
    /// Total frames lost across gaps.
    pub seq_missing: u64,
    /// UI channel events dropped because the UI fell behind (logging is
    /// unaffected; it happens before UI delivery).
    pub ui_events_dropped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_no_gaps() {
        let mut t = SeqTracker::default();
        for s in 0..100u32 {
            assert_eq!(t.on_sequence(s), None);
        }
        assert_eq!(t.gaps, 0);
        assert_eq!(t.missing, 0);
    }

    #[test]
    fn single_gap_counts_missing() {
        let mut t = SeqTracker::default();
        t.on_sequence(10);
        t.on_sequence(11);
        assert_eq!(t.on_sequence(15), Some(3)); // 12,13,14 missing
        assert_eq!(t.gaps, 1);
        assert_eq!(t.missing, 3);
        assert_eq!(t.on_sequence(16), None);
    }

    #[test]
    fn wraparound_is_not_a_gap() {
        let mut t = SeqTracker::default();
        t.on_sequence(u32::MAX - 1);
        assert_eq!(t.on_sequence(u32::MAX), None);
        assert_eq!(t.on_sequence(0), None);
        assert_eq!(t.on_sequence(1), None);
        assert_eq!(t.gaps, 0);
    }

    #[test]
    fn gap_across_wraparound() {
        let mut t = SeqTracker::default();
        t.on_sequence(u32::MAX - 1);
        // Expected u32::MAX, got 2: frames MAX, 0, 1 are missing.
        assert_eq!(t.on_sequence(2), Some(3));
        assert_eq!(t.missing, 3);
    }

    #[test]
    fn reboot_jump_is_reset_not_gap() {
        let mut t = SeqTracker::default();
        t.on_sequence(5_000_000);
        assert_eq!(t.on_sequence(0), None);
        assert_eq!(t.gaps, 0);
        assert_eq!(t.resets, 1);
        // Continues normally from the new baseline.
        assert_eq!(t.on_sequence(1), None);
    }

    #[test]
    fn reconnect_reset_baseline() {
        let mut t = SeqTracker::default();
        t.on_sequence(100);
        t.reset_baseline();
        assert_eq!(
            t.on_sequence(500),
            None,
            "first frame after reconnect is not a gap"
        );
        assert_eq!(t.gaps, 0);
    }

    #[test]
    fn rate_window() {
        let mut r = RateWindow::new(Duration::from_secs(2));
        let t0 = Instant::now();
        for i in 0..100 {
            r.tick(t0 + Duration::from_millis(i * 20)); // 50 Hz for 2 s
        }
        let rate = r.rate(t0 + Duration::from_millis(2000));
        assert!((rate - 50.0).abs() < 2.0, "rate = {rate}");
        // After 3 s of silence everything ages out.
        let rate = r.rate(t0 + Duration::from_secs(5));
        assert_eq!(rate, 0.0);
    }
}
