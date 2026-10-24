//! RTC trim: RTC_CALR conversion and drift learning from `SetRtc` steps
//! (`docs/field-protocol.md` §1, "Time on the board").
//!
//! Sign convention (as `FieldStatus.calr_ppb`): positive ppb = RTC slowed.
//!
//! RTC_CALR smooth calibration (RM0444): over each 2^20 RTCCLK cycles the
//! RTC masks `CALM` pulses and, with `CALP`, inserts 512. With
//! `x = CALM − 512·CALP`, the RTC is slowed by `x / (2^20 + x)`, so one CALM
//! step is about 954 ppb and the range is −488.5..+487.1 ppm.
//!
//! Learning: every accepted `SetRtc` sets the RTC to a whole second (the
//! server's time) and reports its step `new − old`. The first correction
//! (or one after the RTC was invalid) anchors. Later steps accumulate; once
//! the anchor is ≥ 6 h old, the residual drift with the current trim is
//! `e = −Σsteps / elapsed` (positive when the RTC ran fast), the trim moves
//! by `α·e` (α = ½), is clamped to ±200 ppm, and the anchor restarts.

/// Trim limit, ppb.
pub const TRIM_LIMIT_PPB: i32 = 200_000;
/// Minimum anchor age before learning.
pub const MIN_LEARN_ELAPSED_S: u32 = 6 * 3600;
/// A larger single step is treated as a re-set, not drift: restart learning.
pub const MAX_LEARN_STEP_MS: i64 = 60_000;
/// Residuals beyond this are implausible for a 20 ppm crystal: restart.
pub const MAX_RESIDUAL_PPB: i64 = 500_000;

/// Integer division rounding half away from zero (`den > 0`).
fn div_round(num: i64, den: i64) -> i64 {
    if num >= 0 {
        (2 * num + den) / (2 * den)
    } else {
        -((-2 * num + den) / (2 * den))
    }
}

/// RTC_CALR fields for a trim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Calr {
    pub calp: bool,
    pub calm: u16,
}

impl Calr {
    /// Nearest register setting for `ppb` (positive = slower).
    pub fn from_ppb(ppb: i32) -> Self {
        let ppb = ppb.clamp(-TRIM_LIMIT_PPB, TRIM_LIMIT_PPB) as i64;
        let x = div_round(ppb * (1 << 20), 1_000_000_000 - ppb).clamp(-511, 511);
        if x >= 0 {
            Self {
                calp: false,
                calm: x as u16,
            }
        } else {
            Self {
                calp: true,
                calm: (x + 512) as u16,
            }
        }
    }

    /// Effective trim of this setting, ppb (positive = slower).
    pub fn ppb(self) -> i32 {
        let x = self.calm as i64 - if self.calp { 512 } else { 0 };
        div_round(x * 1_000_000_000, (1 << 20) + x) as i32
    }
}

/// Learning state, kept in TAMP backup registers (same domain as the RTC).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrimLearner {
    /// Unix s of the anchoring correction; 0 = none.
    pub anchor_unix_s: u32,
    /// Sum of steps since the anchor, ms.
    pub steps_ms: i32,
}

impl TrimLearner {
    /// Record an applied correction. `rtc_was_valid`: the RTC held a real
    /// time before the step. `new_unix_s`: the whole second just set.
    /// `step_ms`: new − old. Returns the new trim when it changes.
    pub fn on_correction(
        &mut self,
        rtc_was_valid: bool,
        new_unix_s: u32,
        step_ms: i64,
        current_ppb: i32,
    ) -> Option<i32> {
        if !rtc_was_valid
            || self.anchor_unix_s == 0
            || new_unix_s <= self.anchor_unix_s
            || step_ms.abs() > MAX_LEARN_STEP_MS
        {
            self.restart(new_unix_s);
            return None;
        }
        self.steps_ms = (self.steps_ms as i64 + step_ms).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let elapsed_s = new_unix_s - self.anchor_unix_s;
        if elapsed_s < MIN_LEARN_ELAPSED_S {
            return None;
        }
        let residual_ppb = div_round(-(self.steps_ms as i64) * 1_000_000, elapsed_s as i64);
        self.restart(new_unix_s);
        if residual_ppb.abs() > MAX_RESIDUAL_PPB {
            return None;
        }
        let next = (current_ppb as i64 + div_round(residual_ppb, 2))
            .clamp(-(TRIM_LIMIT_PPB as i64), TRIM_LIMIT_PPB as i64) as i32;
        (next != current_ppb).then_some(next)
    }

    fn restart(&mut self, unix_s: u32) {
        self.anchor_unix_s = unix_s;
        self.steps_ms = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values computed in Python from the RM0444 formula.
    #[test]
    fn calr_matches_reference() {
        let cases = [
            (0, false, 0, 0),
            (953, false, 1, 954),
            (1_000, false, 1, 954),
            (-1_000, true, 511, -954),
            (11_000, false, 12, 11_444),
            (20_000, false, 21, 20_027),
            (-20_000, true, 491, -20_028),
            (-31_000, true, 479, -31_472),
            (200_000, false, 210, 200_232),
            (-200_000, true, 302, -200_312),
        ];
        for (ppb, calp, calm, applied) in cases {
            let c = Calr::from_ppb(ppb);
            assert_eq!((c.calp, c.calm), (calp, calm), "ppb {ppb}");
            assert_eq!(c.ppb(), applied, "ppb {ppb}");
        }
        // Clamped to ±200 ppm.
        assert_eq!(Calr::from_ppb(400_000), Calr::from_ppb(200_000));
    }

    const T: u32 = 1_792_800_000;

    #[test]
    fn learns_half_the_residual_after_six_hours() {
        let mut l = TrimLearner::default();
        assert_eq!(l.on_correction(true, T, 0, 0), None);
        // Board 20 ppm fast: 3.5 h later it is 252 ms ahead.
        assert_eq!(l.on_correction(true, T + 12_600, -252, 0), None);
        // 7 h total: another 252 ms.
        assert_eq!(l.on_correction(true, T + 25_200, -252, 0), Some(10_000));
        assert_eq!(l.anchor_unix_s, T + 25_200);
        assert_eq!(l.steps_ms, 0);
        // Converges: residual now 10 ppm.
        assert_eq!(
            l.on_correction(true, T + 25_200 + 36_000, -360, 10_000),
            Some(15_000)
        );
    }

    #[test]
    fn slow_board_trims_negative() {
        let mut l = TrimLearner::default();
        l.on_correction(true, T, 0, 0);
        assert_eq!(l.on_correction(true, T + 21_600, 432, 0), Some(-10_000));
    }

    #[test]
    fn invalid_rtc_or_big_step_restarts() {
        let mut l = TrimLearner::default();
        l.on_correction(true, T, 0, 0);
        assert_eq!(l.on_correction(false, T + 30_000, -500, 0), None);
        assert_eq!(l.anchor_unix_s, T + 30_000);
        assert_eq!(l.on_correction(true, T + 60_000, -3_600_000, 0), None);
        assert_eq!(l.anchor_unix_s, T + 60_000);
        assert_eq!(l.steps_ms, 0);
    }

    #[test]
    fn implausible_residual_is_ignored_and_trim_clamps() {
        let mut l = TrimLearner::default();
        l.on_correction(true, T, 0, 0);
        // 50 s in 7 h = 1984 ppm: rejected (also > MAX_LEARN_STEP_MS? no).
        assert_eq!(l.on_correction(true, T + 25_200, -50_000, 0), None);
        let mut l = TrimLearner::default();
        l.on_correction(true, T, 0, 190_000);
        assert_eq!(l.on_correction(true, T + 21_600, -8_640, 190_000), Some(200_000));
    }
}
