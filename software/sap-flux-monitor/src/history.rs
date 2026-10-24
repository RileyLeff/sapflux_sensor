//! Bounded in-memory plot history.
//!
//! This buffer exists only for rendering. Logging happens in the serial
//! worker before samples reach this buffer, so nothing here (bounds,
//! pruning, decimation) can ever affect what is written to disk.

use std::collections::VecDeque;

/// Hard cap on points kept per series (~20 min at 50 Hz).
pub const MAX_POINTS_PER_SERIES: usize = 60_000;

/// Maximum points handed to the plot per series per frame; older data is
/// stride-decimated for rendering only.
pub const MAX_RENDER_POINTS: usize = 2_000;

/// One plottable series of (t_seconds, value) points, monotonically
/// increasing in t.
#[derive(Debug, Default)]
pub struct SeriesBuf {
    points: VecDeque<[f64; 2]>,
}

impl SeriesBuf {
    /// Append a point; assumes non-decreasing time.
    pub fn push(&mut self, t: f64, v: f64) {
        self.points.push_back([t, v]);
        while self.points.len() > MAX_POINTS_PER_SERIES {
            self.points.pop_front();
        }
    }

    /// Number of stored points.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// True when the buffer holds no points.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Drop everything (new session).
    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// Points with `t >= t_min`, stride-decimated to at most
    /// [`MAX_RENDER_POINTS`]. The last point is always included so the
    /// live edge never lags.
    pub fn window(&self, t_min: f64) -> Vec<[f64; 2]> {
        let start = self.points.partition_point(|p| p[0] < t_min);
        let n = self.points.len() - start;
        if n == 0 {
            return Vec::new();
        }
        let stride = n.div_ceil(MAX_RENDER_POINTS).max(1);
        let mut out: Vec<[f64; 2]> = self
            .points
            .iter()
            .skip(start)
            .step_by(stride)
            .copied()
            .collect();
        let last = *self.points.back().expect("n > 0");
        if out.last() != Some(&last) {
            out.push(last);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_growth() {
        let mut s = SeriesBuf::default();
        for i in 0..(MAX_POINTS_PER_SERIES + 5000) {
            s.push(i as f64, 0.0);
        }
        assert_eq!(s.len(), MAX_POINTS_PER_SERIES);
        // Oldest points were dropped, newest kept.
        assert_eq!(
            s.window(f64::NEG_INFINITY).last(),
            Some(&[(MAX_POINTS_PER_SERIES + 4999) as f64, 0.0])
        );
    }

    #[test]
    fn window_filters_by_time() {
        let mut s = SeriesBuf::default();
        for i in 0..100 {
            s.push(i as f64, i as f64 * 2.0);
        }
        let w = s.window(90.0);
        assert_eq!(w.first(), Some(&[90.0, 180.0]));
        assert_eq!(w.last(), Some(&[99.0, 198.0]));
        assert_eq!(w.len(), 10);
    }

    #[test]
    fn decimation_keeps_live_edge() {
        let mut s = SeriesBuf::default();
        for i in 0..10_000 {
            s.push(i as f64, 1.0);
        }
        let w = s.window(0.0);
        assert!(w.len() <= MAX_RENDER_POINTS + 1);
        assert_eq!(w.last(), Some(&[9999.0, 1.0]));
        assert_eq!(w.first(), Some(&[0.0, 1.0]));
    }
}
