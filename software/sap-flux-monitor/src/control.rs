//! Pulse-control gating.
//!
//! The firmware is the safety boundary; this module only decides what the
//! UI is willing to *ask for*, and it is deliberately conservative:
//!
//! - nothing mutating is ever sent automatically (no auto-retry, ever);
//! - StartBaseline/Arm/Fire additionally require the board's Hello to
//!   advertise [`CAP_HEATER_CONTROL`] — heater-incapable firmware disables
//!   every heater control (Abort stays available while the board reports an
//!   active run state);
//! - Arm requires a fresh board status for the matching run in a
//!   Baseline/Armed state, RTC valid, PGOOD, recent valid ADC frames, and
//!   board-reported cooldown of zero;
//! - Fire additionally requires a successful matching ArmPulse ack *and* an
//!   Armed status, and each fire nonce is one-use for the process lifetime;
//! - a disconnect clears all local arm/fire intent.

use sap_flux_protocol::{
    Ack, BoardState, Hello, Status, CAP_HEATER_CONTROL, FLAG_PGOOD, FLAG_RTC_VALID, MSG_ARM_PULSE,
    MSG_FIRE_PULSE,
};
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

/// The only pulse durations the UI can request.
pub const PULSE_CHOICES_MS: [u16; 3] = [500, 1000, 1500];
/// Default pulse duration.
pub const DEFAULT_PULSE_MS: u16 = 500;

/// Allowed baseline duration (seconds).
pub const BASELINE_S_RANGE: RangeInclusive<u32> = 10..=600;
/// Allowed recovery duration (seconds).
pub const RECOVERY_S_RANGE: RangeInclusive<u32> = 30..=1800;
/// Allowed sample rate (complete frames per second).
pub const SAMPLE_HZ_RANGE: RangeInclusive<u16> = 1..=100;

/// A status older than this no longer authorizes anything.
pub const STATUS_FRESH: Duration = Duration::from_secs(5);
/// "Recent valid ADC frames" window for arm gating.
pub const SAMPLE_FRESH: Duration = Duration::from_secs(2);

/// Editable baseline-run parameters (bounded by the ranges above).
#[derive(Debug, Clone, Copy)]
pub struct BaselineParams {
    /// Pre-pulse baseline duration, seconds.
    pub baseline_s: u32,
    /// Post-pulse recovery duration, seconds.
    pub recovery_s: u32,
    /// Requested complete-frame sample rate.
    pub sample_hz: u16,
}

impl Default for BaselineParams {
    fn default() -> Self {
        Self {
            baseline_s: 60,
            recovery_s: 180,
            sample_hz: 50,
        }
    }
}

impl BaselineParams {
    /// Clamp all fields into their permitted ranges.
    pub fn clamp(&mut self) {
        self.baseline_s = self
            .baseline_s
            .clamp(*BASELINE_S_RANGE.start(), *BASELINE_S_RANGE.end());
        self.recovery_s = self
            .recovery_s
            .clamp(*RECOVERY_S_RANGE.start(), *RECOVERY_S_RANGE.end());
        self.sample_hz = self
            .sample_hz
            .clamp(*SAMPLE_HZ_RANGE.start(), *SAMPLE_HZ_RANGE.end());
    }
}

/// Where we are in the arm/fire handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmPhase {
    /// No arm intent.
    Idle,
    /// ArmPulse sent, waiting for its ack.
    ArmRequested {
        /// Nonce of the outstanding ArmPulse.
        nonce: u64,
        /// Run the arm applies to.
        run_id: u32,
        /// Requested duration.
        pulse_ms: u16,
    },
    /// ArmPulse acked with result 0; fire becomes possible once the board
    /// also reports the Armed state.
    ArmAcked {
        /// Run the arm applies to.
        run_id: u32,
        /// Acked duration.
        pulse_ms: u16,
    },
    /// FirePulse sent, waiting for its ack. No retry happens in any case.
    FireRequested {
        /// Nonce of the sent FirePulse (already marked used).
        nonce: u64,
        /// Run being fired.
        run_id: u32,
    },
}

/// UI-side pulse-control state machine. All inputs are explicit and all
/// time comparisons take `now` so the logic is unit-testable.
#[derive(Debug)]
pub struct PulseControl {
    /// Editable run parameters.
    pub params: BaselineParams,
    /// Selected pulse duration (one of [`PULSE_CHOICES_MS`]).
    pub pulse_ms: u16,
    /// Run id we started with StartBaseline this connection, if any.
    pub active_run: Option<u32>,
    baseline_ready_at: Option<Instant>,
    /// Arm/fire handshake state.
    pub phase: ArmPhase,
    used_fire_nonces: Vec<u64>,
    capabilities: Option<u32>,
    last_status: Option<Status>,
    last_status_at: Option<Instant>,
    last_sample_flags: u16,
    last_valid_sample_at: Option<Instant>,
}

impl Default for PulseControl {
    fn default() -> Self {
        Self {
            params: BaselineParams::default(),
            pulse_ms: DEFAULT_PULSE_MS,
            active_run: None,
            baseline_ready_at: None,
            phase: ArmPhase::Idle,
            used_fire_nonces: Vec::new(),
            capabilities: None,
            last_status: None,
            last_status_at: None,
            last_sample_flags: 0,
            last_valid_sample_at: None,
        }
    }
}

impl PulseControl {
    /// The most recent status, if fresh enough to authorize anything.
    pub fn fresh_status(&self, now: Instant) -> Option<&Status> {
        let at = self.last_status_at?;
        if now.duration_since(at) <= STATUS_FRESH {
            self.last_status.as_ref()
        } else {
            None
        }
    }

    /// Most recent status regardless of freshness (for display).
    pub fn last_status(&self) -> Option<&Status> {
        self.last_status.as_ref()
    }

    /// Record a Status frame.
    pub fn note_status(&mut self, status: Status, now: Instant) {
        // If the board left the Armed state (abort, fault, pulse complete),
        // any local armed intent is stale.
        if let ArmPhase::ArmAcked { run_id, .. } | ArmPhase::ArmRequested { run_id, .. } =
            self.phase
        {
            if status.run_id != run_id
                || !matches!(status.state, BoardState::Baseline | BoardState::Armed)
            {
                self.phase = ArmPhase::Idle;
            }
        }
        self.last_status = Some(status);
        self.last_status_at = Some(now);
    }

    /// Record a Hello frame (the firmware's capability claim).
    pub fn note_hello(&mut self, hello: &Hello) {
        self.capabilities = Some(hello.capabilities);
    }

    /// True only when the latest Hello explicitly advertises
    /// [`CAP_HEATER_CONTROL`]. No Hello yet means no heater controls.
    pub fn heater_capable(&self) -> bool {
        self.capabilities
            .is_some_and(|c| c & CAP_HEATER_CONTROL != 0)
    }

    /// A Hello was received and it lacks heater control: show why the
    /// heater controls are disabled.
    pub fn heater_disabled_by_firmware(&self) -> bool {
        self.capabilities.is_some() && !self.heater_capable()
    }

    /// Record a Sample frame (its flags and whether it was usable).
    pub fn note_sample(&mut self, flags: u16, sample_valid: bool, now: Instant) {
        self.last_sample_flags = flags;
        if sample_valid {
            self.last_valid_sample_at = Some(now);
        }
    }

    /// Record an Ack frame. Advances the arm/fire handshake when the ack
    /// matches the outstanding nonce; a rejection returns to Idle (the UI
    /// never retries on its own).
    pub fn note_ack(&mut self, ack: &Ack) {
        match self.phase {
            ArmPhase::ArmRequested {
                nonce,
                run_id,
                pulse_ms,
            } if ack.command_nonce == nonce && ack.command_type == MSG_ARM_PULSE => {
                self.phase = if ack.result == 0 {
                    ArmPhase::ArmAcked { run_id, pulse_ms }
                } else {
                    ArmPhase::Idle
                };
            }
            ArmPhase::FireRequested { nonce, .. }
                if ack.command_nonce == nonce && ack.command_type == MSG_FIRE_PULSE =>
            {
                // Accepted or rejected, the armed intent is consumed either
                // way; a new pulse requires a fresh arm cycle.
                self.phase = ArmPhase::Idle;
            }
            _ => {}
        }
    }

    /// StartBaseline may be requested: heater-capable firmware, fresh
    /// status, SafeIdle, no fault, and board-reported cooldown of zero.
    pub fn can_start_baseline(&self, now: Instant) -> bool {
        if !self.heater_capable() {
            return false;
        }
        match self.fresh_status(now) {
            Some(st) => {
                st.state == BoardState::SafeIdle
                    && st.fault_code == 0
                    && st.cooldown_remaining_ms == 0
            }
            None => false,
        }
    }

    /// ArmPulse may be requested. See module docs for the full condition
    /// list.
    pub fn can_arm(&self, now: Instant) -> bool {
        if !self.heater_capable() {
            return false;
        }
        let Some(st) = self.fresh_status(now) else {
            return false;
        };
        let Some(run) = self.active_run else {
            return false;
        };
        let baseline_complete = self.baseline_ready_at.is_some_and(|ready| now >= ready);
        let rtc_valid = self.last_sample_flags & FLAG_RTC_VALID != 0 && st.rtc_unix_ms >= 0;
        let pgood = self.last_sample_flags & FLAG_PGOOD != 0;
        let recent_valid_sample = self
            .last_valid_sample_at
            .is_some_and(|at| now.duration_since(at) <= SAMPLE_FRESH);
        st.run_id == run
            && matches!(st.state, BoardState::Baseline | BoardState::Armed)
            && st.fault_code == 0
            && st.cooldown_remaining_ms == 0
            && rtc_valid
            && pgood
            && recent_valid_sample
            && baseline_complete
            && !matches!(self.phase, ArmPhase::FireRequested { .. })
    }

    /// FirePulse may be *offered* (confirmation modal): a successful
    /// matching arm ack plus a fresh Armed status for the same run.
    pub fn can_fire(&self, now: Instant) -> bool {
        if !self.heater_capable() {
            return false;
        }
        let ArmPhase::ArmAcked { run_id, .. } = self.phase else {
            return false;
        };
        let Some(st) = self.fresh_status(now) else {
            return false;
        };
        st.state == BoardState::Armed && st.run_id == run_id && st.fault_code == 0
    }

    /// Abort should be prominent: board is in any active-run state.
    pub fn abort_relevant(&self) -> bool {
        matches!(
            self.last_status.as_ref().map(|s| s.state),
            Some(
                BoardState::Baseline
                    | BoardState::Armed
                    | BoardState::Pulsing
                    | BoardState::Recovery
            )
        )
    }

    /// Record that StartBaseline was sent for `run_id`. The host-side timer
    /// prevents offering Arm before the board's requested baseline can end;
    /// the board remains authoritative and checks independently.
    pub fn on_start_baseline_sent(
        &mut self,
        run_id: u32,
        baseline_duration: Duration,
        now: Instant,
    ) {
        self.active_run = Some(run_id);
        self.baseline_ready_at = Some(now + baseline_duration);
        self.phase = ArmPhase::Idle;
    }

    /// Time remaining before the locally requested baseline can be armed.
    pub fn baseline_remaining(&self, now: Instant) -> Option<Duration> {
        self.baseline_ready_at
            .map(|ready| ready.saturating_duration_since(now))
    }

    /// Record that ArmPulse was sent.
    pub fn on_arm_sent(&mut self, nonce: u64, run_id: u32, pulse_ms: u16) {
        self.phase = ArmPhase::ArmRequested {
            nonce,
            run_id,
            pulse_ms,
        };
    }

    /// Whether a fire nonce has already been consumed this process.
    pub fn fire_nonce_used(&self, nonce: u64) -> bool {
        self.used_fire_nonces.contains(&nonce)
    }

    /// Record that FirePulse was sent. Returns `false` (and records
    /// nothing) if the nonce was already used — the caller must not send.
    #[must_use]
    pub fn on_fire_sent(&mut self, nonce: u64, run_id: u32) -> bool {
        if self.fire_nonce_used(nonce) {
            return false;
        }
        self.used_fire_nonces.push(nonce);
        self.phase = ArmPhase::FireRequested { nonce, run_id };
        true
    }

    /// Record that Abort was sent: all local arm/fire intent is dropped.
    pub fn on_abort_sent(&mut self) {
        self.phase = ArmPhase::Idle;
    }

    /// Connection dropped (or is being re-established): clear every piece
    /// of local run/arm/fire intent and all cached board state. Used fire
    /// nonces are intentionally retained — one-use is per process.
    pub fn on_disconnect(&mut self) {
        self.active_run = None;
        self.baseline_ready_at = None;
        self.phase = ArmPhase::Idle;
        self.capabilities = None;
        self.last_status = None;
        self.last_status_at = None;
        self.last_sample_flags = 0;
        self.last_valid_sample_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sap_flux_protocol::{CAP_ADC, CAP_RTC, CAP_V24_MONITOR, FLAG_EXCITATION_ACTIVE};

    const GOOD_FLAGS: u16 = FLAG_RTC_VALID | FLAG_PGOOD | FLAG_EXCITATION_ACTIVE;
    /// Everything the live heater-incapable firmware advertises.
    const CAPS_NO_HEATER: u32 = CAP_RTC | CAP_ADC | CAP_V24_MONITOR;
    const CAPS_FULL: u32 = CAPS_NO_HEATER | CAP_HEATER_CONTROL;

    fn hello(capabilities: u32) -> Hello {
        Hello {
            board_id: 1,
            firmware_version: (1 << 8) | 2,
            capabilities,
        }
    }

    fn status(state: BoardState, run_id: u32, cooldown_ms: u32) -> Status {
        Status {
            monotonic_us: 1,
            rtc_unix_ms: 1_772_000_000_000,
            run_id,
            state,
            fault_code: 0,
            pulse_duration_ms: 0,
            cooldown_remaining_ms: cooldown_ms,
        }
    }

    /// Drive a control to the "ready to arm" state.
    fn armed_ready(now: Instant) -> PulseControl {
        let mut c = PulseControl::default();
        c.note_hello(&hello(CAPS_FULL));
        c.note_status(status(BoardState::SafeIdle, 0, 0), now);
        assert!(c.can_start_baseline(now));
        c.on_start_baseline_sent(42, Duration::ZERO, now);
        c.note_status(status(BoardState::Baseline, 42, 0), now);
        c.note_sample(GOOD_FLAGS, true, now);
        c
    }

    #[test]
    fn happy_path_arm_then_fire() {
        let now = Instant::now();
        let mut c = armed_ready(now);
        assert!(c.can_arm(now));
        assert!(!c.can_fire(now), "fire requires arm ack + Armed status");

        c.on_arm_sent(1001, 42, 500);
        assert!(!c.can_fire(now), "no fire before ack");
        c.note_ack(&Ack {
            command_type: MSG_ARM_PULSE,
            result: 0,
            command_nonce: 1001,
            run_id: 42,
        });
        assert!(!c.can_fire(now), "no fire before Armed status");
        c.note_status(status(BoardState::Armed, 42, 0), now);
        assert!(c.can_fire(now));

        assert!(c.on_fire_sent(2002, 42));
        assert!(!c.can_fire(now), "fire intent consumed on send");
        c.note_ack(&Ack {
            command_type: MSG_FIRE_PULSE,
            result: 0,
            command_nonce: 2002,
            run_id: 42,
        });
        assert_eq!(c.phase, ArmPhase::Idle);
    }

    #[test]
    fn arm_is_not_offered_before_requested_baseline_finishes() {
        let started = Instant::now();
        let mut c = PulseControl::default();
        c.note_hello(&hello(CAPS_FULL));
        c.note_status(status(BoardState::SafeIdle, 0, 0), started);
        c.on_start_baseline_sent(42, Duration::from_secs(60), started);

        let almost_ready = started + Duration::from_secs(59);
        c.note_status(status(BoardState::Baseline, 42, 0), almost_ready);
        c.note_sample(GOOD_FLAGS, true, almost_ready);
        assert!(!c.can_arm(almost_ready));
        assert_eq!(
            c.baseline_remaining(almost_ready),
            Some(Duration::from_secs(1))
        );

        let ready = started + Duration::from_secs(60);
        c.note_status(status(BoardState::Baseline, 42, 0), ready);
        c.note_sample(GOOD_FLAGS, true, ready);
        assert!(c.can_arm(ready));
        assert_eq!(c.baseline_remaining(ready), Some(Duration::ZERO));
    }

    #[test]
    fn fire_nonce_is_one_use() {
        let now = Instant::now();
        let mut c = armed_ready(now);
        c.on_arm_sent(1, 42, 500);
        c.note_ack(&Ack {
            command_type: MSG_ARM_PULSE,
            result: 0,
            command_nonce: 1,
            run_id: 42,
        });
        c.note_status(status(BoardState::Armed, 42, 0), now);
        assert!(c.on_fire_sent(77, 42));
        // Same nonce can never be sent again, no matter the state.
        assert!(!c.on_fire_sent(77, 42));
        assert!(c.fire_nonce_used(77));
        // Even after a full disconnect/reconnect cycle.
        c.on_disconnect();
        assert!(!c.on_fire_sent(77, 42));
    }

    #[test]
    fn arm_blocked_without_each_precondition() {
        let now = Instant::now();

        // Missing rtc_valid.
        let mut c = armed_ready(now);
        c.note_sample(FLAG_PGOOD, true, now);
        assert!(!c.can_arm(now));

        // Missing pgood.
        let mut c = armed_ready(now);
        c.note_sample(FLAG_RTC_VALID, true, now);
        assert!(!c.can_arm(now));

        // No valid sample ever received (flags fine, conversions invalid).
        let mut c = PulseControl::default();
        c.note_hello(&hello(CAPS_FULL));
        c.note_status(status(BoardState::SafeIdle, 0, 0), now);
        c.on_start_baseline_sent(42, Duration::ZERO, now);
        c.note_status(status(BoardState::Baseline, 42, 0), now);
        c.note_sample(GOOD_FLAGS, false, now);
        assert!(!c.can_arm(now));

        // Stale valid sample (status is still fresh at +3 s; only the
        // sample recency check fails).
        let c = armed_ready(now);
        assert!(!c.can_arm(now + SAMPLE_FRESH + Duration::from_secs(1)));

        // Nonzero cooldown.
        let mut c = armed_ready(now);
        c.note_status(status(BoardState::Baseline, 42, 30_000), now);
        assert!(!c.can_arm(now));

        // Wrong run id.
        let mut c = armed_ready(now);
        c.note_status(status(BoardState::Baseline, 43, 0), now);
        assert!(!c.can_arm(now));

        // Wrong state.
        let mut c = armed_ready(now);
        c.note_status(status(BoardState::Cooldown, 42, 1000), now);
        assert!(!c.can_arm(now));

        // Fault latched.
        let mut c = armed_ready(now);
        let mut st = status(BoardState::Baseline, 42, 0);
        st.fault_code = 3;
        c.note_status(st, now);
        assert!(!c.can_arm(now));

        // Stale status.
        let c = armed_ready(now);
        assert!(!c.can_arm(now + STATUS_FRESH + Duration::from_secs(1)));

        // No StartBaseline sent by us.
        let mut c = PulseControl::default();
        c.note_hello(&hello(CAPS_FULL));
        c.note_status(status(BoardState::Baseline, 42, 0), now);
        c.note_sample(GOOD_FLAGS, true, now);
        assert!(!c.can_arm(now));
    }

    #[test]
    fn heater_capability_gates_start_arm_and_fire() {
        let now = Instant::now();

        // Heater-incapable Hello (the live firmware): everything otherwise
        // authorizing is present, yet nothing heater-related is permitted.
        let mut c = PulseControl::default();
        c.note_hello(&hello(CAPS_NO_HEATER));
        assert!(c.heater_disabled_by_firmware());
        c.note_status(status(BoardState::SafeIdle, 0, 0), now);
        assert!(!c.can_start_baseline(now), "no heater cap -> no baseline");
        c.on_start_baseline_sent(42, Duration::ZERO, now);
        c.note_status(status(BoardState::Baseline, 42, 0), now);
        c.note_sample(GOOD_FLAGS, true, now);
        assert!(!c.can_arm(now), "no heater cap -> no arm");
        c.on_arm_sent(1, 42, 500);
        c.note_ack(&Ack {
            command_type: MSG_ARM_PULSE,
            result: 0,
            command_nonce: 1,
            run_id: 42,
        });
        c.note_status(status(BoardState::Armed, 42, 0), now);
        assert!(!c.can_fire(now), "no heater cap -> no fire");

        // Abort stays relevant while the board itself reports an active
        // state, capability or not.
        assert!(c.abort_relevant());

        // No Hello at all: also no heater controls.
        let mut c = PulseControl::default();
        c.note_status(status(BoardState::SafeIdle, 0, 0), now);
        assert!(!c.can_start_baseline(now), "no Hello -> no baseline");
        assert!(!c.heater_disabled_by_firmware(), "no Hello -> no banner");

        // A heater-capable Hello enables the same path.
        let mut c = PulseControl::default();
        c.note_hello(&hello(CAPS_FULL));
        assert!(!c.heater_disabled_by_firmware());
        c.note_status(status(BoardState::SafeIdle, 0, 0), now);
        assert!(c.can_start_baseline(now));
    }

    #[test]
    fn capabilities_cleared_on_disconnect() {
        let now = Instant::now();
        let mut c = PulseControl::default();
        c.note_hello(&hello(CAPS_FULL));
        assert!(c.heater_capable());
        c.on_disconnect();
        assert!(!c.heater_capable(), "capability claim dies with the link");
        c.note_status(status(BoardState::SafeIdle, 0, 0), now);
        assert!(
            !c.can_start_baseline(now),
            "reconnect requires a fresh Hello before heater controls return"
        );
    }

    #[test]
    fn arm_ack_rejection_returns_to_idle() {
        let now = Instant::now();
        let mut c = armed_ready(now);
        c.on_arm_sent(5, 42, 500);
        c.note_ack(&Ack {
            command_type: MSG_ARM_PULSE,
            result: 2,
            command_nonce: 5,
            run_id: 42,
        });
        assert_eq!(c.phase, ArmPhase::Idle);
        assert!(!c.can_fire(now));
    }

    #[test]
    fn mismatched_ack_ignored() {
        let now = Instant::now();
        let mut c = armed_ready(now);
        c.on_arm_sent(5, 42, 500);
        // Wrong nonce.
        c.note_ack(&Ack {
            command_type: MSG_ARM_PULSE,
            result: 0,
            command_nonce: 6,
            run_id: 42,
        });
        assert!(matches!(c.phase, ArmPhase::ArmRequested { .. }));
        // Wrong command type.
        c.note_ack(&Ack {
            command_type: MSG_FIRE_PULSE,
            result: 0,
            command_nonce: 5,
            run_id: 42,
        });
        assert!(matches!(c.phase, ArmPhase::ArmRequested { .. }));
    }

    #[test]
    fn board_leaving_armed_state_clears_intent() {
        let now = Instant::now();
        let mut c = armed_ready(now);
        c.on_arm_sent(5, 42, 500);
        c.note_ack(&Ack {
            command_type: MSG_ARM_PULSE,
            result: 0,
            command_nonce: 5,
            run_id: 42,
        });
        c.note_status(status(BoardState::Armed, 42, 0), now);
        assert!(c.can_fire(now));
        // Board drops back to SafeIdle (e.g. board-side timeout/abort).
        c.note_status(status(BoardState::SafeIdle, 42, 0), now);
        assert_eq!(c.phase, ArmPhase::Idle);
        assert!(!c.can_fire(now));
    }

    #[test]
    fn disconnect_clears_all_intent_and_requires_fresh_everything() {
        let now = Instant::now();
        let mut c = armed_ready(now);
        c.on_arm_sent(5, 42, 500);
        c.note_ack(&Ack {
            command_type: MSG_ARM_PULSE,
            result: 0,
            command_nonce: 5,
            run_id: 42,
        });
        c.note_status(status(BoardState::Armed, 42, 0), now);
        assert!(c.can_fire(now));

        c.on_disconnect();
        assert_eq!(c.phase, ArmPhase::Idle);
        assert_eq!(c.active_run, None);
        assert!(!c.can_fire(now));
        assert!(!c.can_arm(now));
        assert!(
            !c.can_start_baseline(now),
            "requires a fresh post-reconnect status"
        );

        // Even fresh board state after reconnect is not enough to arm
        // without a fresh explicit StartBaseline from the user.
        c.note_status(status(BoardState::Baseline, 42, 0), now);
        c.note_sample(GOOD_FLAGS, true, now);
        assert!(!c.can_arm(now), "active_run was cleared on disconnect");
    }

    #[test]
    fn params_clamped_to_conservative_bounds() {
        let mut p = BaselineParams {
            baseline_s: 0,
            recovery_s: 1_000_000,
            sample_hz: 0,
        };
        p.clamp();
        assert_eq!(p.baseline_s, *BASELINE_S_RANGE.start());
        assert_eq!(p.recovery_s, *RECOVERY_S_RANGE.end());
        assert_eq!(p.sample_hz, *SAMPLE_HZ_RANGE.start());
    }

    #[test]
    fn defaults_match_spec() {
        let p = BaselineParams::default();
        assert_eq!(p.baseline_s, 60);
        assert_eq!(p.recovery_s, 180);
        assert_eq!(p.sample_hz, 50);
        assert_eq!(DEFAULT_PULSE_MS, 500);
        assert_eq!(PULSE_CHOICES_MS, [500, 1000, 1500]);
    }
}
