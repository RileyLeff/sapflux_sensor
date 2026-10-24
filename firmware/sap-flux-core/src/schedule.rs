//! Autonomous pulse planner (`docs/field-protocol.md` §1, `SetSchedule`).
//!
//! The planner never touches hardware or the heater. Each [`Planner::poll`]
//! returns at most one [`Action`] that the firmware executes through the very
//! same [`Machine`](crate::pulse::Machine) calls a host uses (`start_baseline`,
//! `arm`, `request_fire` → persisted lockout → hardware one-pulse, `abort`),
//! so every safety layer of the pulse state machine applies unchanged.
//!
//! Slots: heater-on times `t` (Unix s) with `(t - phase_s) mod interval_s ==
//! 0`; baseline starts `baseline_s` earlier. With the RTC invalid, slots are
//! `anchor + k·interval` on the monotonic clock (`k ≥ 1`), where the anchor
//! is the moment the planner entered that mode (boot, a schedule change, or
//! the RTC becoming invalid), so the first such pulse is one full interval
//! after the anchor.
//!
//! A slot whose baseline start is missed by more than [`START_SLACK_MS`]
//! (board busy, in cooldown, or a host run in progress) is skipped. A run
//! that cannot arm and fire within [`FIRE_GRACE_MS`] of its slot (health,
//! PGOOD, ADC) is aborted. Disabling the schedule or losing the lease aborts
//! a run that has not fired yet; a fired run always finishes its recovery.

use crate::pulse::{BaselineRequest, PulseDuration, State};
use sap_flux_protocol::Schedule;

/// How late a baseline may start and still use its slot.
pub const START_SLACK_MS: u64 = 2_000;
/// How long after the slot's heater-on time arm/fire may keep retrying
/// before the run is aborted. Covers a start up to [`START_SLACK_MS`] late
/// (the machine enforces the full baseline) plus health retries.
pub const FIRE_GRACE_MS: u64 = START_SLACK_MS + 1_000;

/// High bit marks autonomous run ids; host runs choose their own.
pub const SCHEDULED_RUN_FLAG: u32 = 0x8000_0000;

/// What the firmware should do now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    None,
    /// `Machine::start_baseline`. Report rejection with
    /// [`Planner::start_rejected`].
    StartBaseline(BaselineRequest),
    /// `Machine::arm`.
    Arm {
        run_id: u32,
        duration: PulseDuration,
    },
    /// `Machine::request_fire` and the persisted-lockout fire sequence.
    Fire {
        run_id: u32,
        nonce: u64,
    },
    /// `Machine::abort`.
    Abort {
        run_id: u32,
    },
}

/// Snapshot of the world for one [`Planner::poll`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Inputs {
    /// Monotonic ms.
    pub now_ms: u64,
    /// RTC time if valid.
    pub unix_ms: Option<i64>,
    /// Pulse machine state and run id.
    pub state: State,
    pub machine_run_id: u32,
    /// Do not start new runs (e.g. a latched persistence fault).
    pub hold: bool,
}

/// One planned heater-on slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Slot {
    /// Heater-on time on the monotonic clock.
    pub fire_at_ms: u64,
    /// Heater-on time in Unix s, when planned on the RTC.
    pub fire_unix_s: Option<u32>,
    key: SlotKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotKey {
    Unix(u64),
    Mono(u64),
}

/// The run the planner started and still follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveRun {
    pub run_id: u32,
    pub fire_at_ms: u64,
    pub fire_unix_s: Option<u32>,
    pub fired: bool,
    /// Planned on the monotonic clock (RTC invalid): window `TIME_INVALID`.
    pub time_invalid: bool,
    /// Schedule captured at baseline start.
    pub schedule: Schedule,
}

#[derive(Clone, Copy, Debug)]
pub struct Planner {
    schedule: Schedule,
    lease_until_unix_s: u32,
    run_prefix: u32,
    seq: u16,
    mono_anchor_ms: Option<u64>,
    last_key: Option<SlotKey>,
    run: Option<ActiveRun>,
}

/// Smallest `t ≥ x` with `(t - phase) mod interval == 0`.
pub fn next_aligned(x: i64, phase: u32, interval: u32) -> i64 {
    let interval = interval.max(1) as i64;
    let r = (x - phase as i64).rem_euclid(interval);
    if r == 0 {
        x
    } else {
        x + (interval - r)
    }
}

/// Fire nonce used for a scheduled run (distinct per run).
pub const fn scheduled_fire_nonce(run_id: u32) -> u64 {
    0x5346_5343_0000_0000 | run_id as u64
}

impl Planner {
    /// `boot_count` makes run ids unique across reboots.
    pub fn new(schedule: Schedule, lease_until_unix_s: u32, boot_count: u32) -> Self {
        Self {
            schedule,
            lease_until_unix_s,
            run_prefix: SCHEDULED_RUN_FLAG | ((boot_count & 0x7fff) << 16),
            seq: 0,
            mono_anchor_ms: None,
            last_key: None,
            run: None,
        }
    }

    pub fn schedule(&self) -> Schedule {
        self.schedule
    }

    pub fn lease_until_unix_s(&self) -> u32 {
        self.lease_until_unix_s
    }

    pub fn active_run(&self) -> Option<ActiveRun> {
        self.run
    }

    /// Replace the schedule. A changed schedule restarts the monotonic
    /// anchor, so an RTC-less board waits a full interval after any change.
    pub fn set_schedule(&mut self, schedule: Schedule, now_ms: u64) {
        if schedule != self.schedule {
            self.schedule = schedule;
            if self.mono_anchor_ms.is_some() {
                self.mono_anchor_ms = Some(now_ms);
            }
        }
    }

    pub fn set_lease(&mut self, lease_until_unix_s: u32) {
        self.lease_until_unix_s = lease_until_unix_s;
    }

    /// Whether the lease currently permits pulsing (always true without
    /// `REQUIRE_LEASE`; needs a valid RTC with it).
    pub fn lease_ok(&self, unix_ms: Option<i64>) -> bool {
        if !self.schedule.requires_lease() {
            return true;
        }
        match unix_ms {
            Some(ms) => ms.div_euclid(1000) < self.lease_until_unix_s as i64,
            None => false,
        }
    }

    /// Whether the lease is valid now (for `FieldStatus`), regardless of the
    /// `REQUIRE_LEASE` flag.
    pub fn lease_valid(&self, unix_ms: Option<i64>) -> bool {
        matches!(unix_ms, Some(ms) if ms.div_euclid(1000) < self.lease_until_unix_s as i64)
    }

    /// The firmware could not start the run returned by the last poll.
    pub fn start_rejected(&mut self) {
        self.run = None;
    }

    /// Next heater-on time in Unix s for `FieldStatus` (0 when disabled,
    /// lease-blocked or when the RTC is invalid).
    pub fn next_pulse_unix_s(&self, now_ms: u64, unix_ms: Option<i64>) -> u32 {
        if !self.schedule.is_enabled() || !self.lease_ok(unix_ms) {
            return 0;
        }
        if let Some(run) = self.run {
            if !run.fired {
                return run.fire_unix_s.unwrap_or(0);
            }
        }
        self.next_slot(now_ms, unix_ms)
            .and_then(|s| s.fire_unix_s)
            .unwrap_or(0)
    }

    /// Next slot not yet used, if the schedule is enabled.
    pub fn next_slot(&self, now_ms: u64, unix_ms: Option<i64>) -> Option<Slot> {
        if !self.schedule.is_enabled() {
            return None;
        }
        let baseline_ms = self.schedule.baseline_s as u64 * 1000;
        let interval_s = self.schedule.interval_s.max(1);
        match unix_ms {
            Some(unix_ms) => {
                let earliest_ms = unix_ms - START_SLACK_MS as i64 + baseline_ms as i64;
                let earliest_s =
                    earliest_ms.div_euclid(1000) + (earliest_ms.rem_euclid(1000) != 0) as i64;
                let mut t = next_aligned(earliest_s, self.schedule.phase_s, interval_s);
                if let Some(SlotKey::Unix(last)) = self.last_key {
                    if t <= last as i64 {
                        t = next_aligned(last as i64 + 1, self.schedule.phase_s, interval_s);
                    }
                }
                let delta_ms = t * 1000 - unix_ms;
                let fire_at_ms = if delta_ms >= 0 {
                    now_ms.saturating_add(delta_ms as u64)
                } else {
                    now_ms.saturating_sub(delta_ms.unsigned_abs())
                };
                Some(Slot {
                    fire_at_ms,
                    fire_unix_s: u32::try_from(t).ok(),
                    key: SlotKey::Unix(t.max(0) as u64),
                })
            }
            None => {
                let anchor = self.mono_anchor_ms.unwrap_or(now_ms);
                let interval_ms = interval_s as u64 * 1000;
                let earliest = (now_ms + baseline_ms).saturating_sub(START_SLACK_MS);
                let mut k = earliest.saturating_sub(anchor).div_ceil(interval_ms).max(1);
                if let Some(SlotKey::Mono(last)) = self.last_key {
                    if k <= last {
                        k = last + 1;
                    }
                }
                Some(Slot {
                    fire_at_ms: anchor.saturating_add(k.saturating_mul(interval_ms)),
                    fire_unix_s: None,
                    key: SlotKey::Mono(k),
                })
            }
        }
    }

    pub fn poll(&mut self, inputs: Inputs) -> Action {
        let now = inputs.now_ms;
        match (inputs.unix_ms, self.mono_anchor_ms) {
            (Some(_), Some(_)) => {
                // RTC became valid: plan on Unix time from now on.
                self.mono_anchor_ms = None;
                if matches!(self.last_key, Some(SlotKey::Mono(_))) {
                    self.last_key = None;
                }
            }
            (None, None) => {
                self.mono_anchor_ms = Some(now);
                if matches!(self.last_key, Some(SlotKey::Unix(_))) {
                    self.last_key = None;
                }
            }
            _ => {}
        }

        if let Some(mut run) = self.run {
            if inputs.machine_run_id != run.run_id {
                // Aborted, cleared, or taken over: the run is over.
                self.run = None;
            } else {
                if matches!(
                    inputs.state,
                    State::Pulsing | State::Recovery | State::Cooldown | State::Fault
                ) {
                    run.fired = true;
                    self.run = Some(run);
                }
                if run.fired {
                    return Action::None;
                }
                let lease_ok = self.lease_ok(inputs.unix_ms);
                if !self.schedule.is_enabled()
                    || !lease_ok
                    || now > run.fire_at_ms.saturating_add(FIRE_GRACE_MS)
                {
                    return Action::Abort { run_id: run.run_id };
                }
                if now < run.fire_at_ms {
                    return Action::None;
                }
                return match inputs.state {
                    State::Baseline => match PulseDuration::from_ms(run.schedule.pulse_duration_ms)
                    {
                        Some(duration) => Action::Arm {
                            run_id: run.run_id,
                            duration,
                        },
                        None => Action::Abort { run_id: run.run_id },
                    },
                    State::Armed => Action::Fire {
                        run_id: run.run_id,
                        nonce: scheduled_fire_nonce(run.run_id),
                    },
                    _ => Action::None,
                };
            }
        }

        if inputs.hold || !self.lease_ok(inputs.unix_ms) {
            return Action::None;
        }
        let Some(slot) = self.next_slot(now, inputs.unix_ms) else {
            return Action::None;
        };
        let baseline_ms = self.schedule.baseline_s as u64 * 1000;
        let start_at = slot.fire_at_ms.saturating_sub(baseline_ms);
        if now < start_at {
            return Action::None;
        }
        // The slot is due: use it or lose it.
        self.last_key = Some(slot.key);
        if inputs.state != State::SafeIdle || now > start_at + START_SLACK_MS {
            return Action::None;
        }
        self.seq = if self.seq == u16::MAX {
            1
        } else {
            self.seq + 1
        };
        let run_id = self.run_prefix | self.seq as u32;
        let time_invalid = inputs.unix_ms.is_none();
        self.run = Some(ActiveRun {
            run_id,
            fire_at_ms: slot.fire_at_ms,
            fire_unix_s: slot.fire_unix_s,
            fired: false,
            time_invalid,
            schedule: self.schedule,
        });
        Action::StartBaseline(BaselineRequest {
            run_id,
            baseline_duration_ms: baseline_ms as u32,
            recovery_duration_ms: self.schedule.recovery_s as u32 * 1000,
            sample_hz: self.schedule.sample_hz,
            require_rtc: !time_invalid,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::pulse::{Effect, Health, Machine, COOLDOWN_MS};
    use std::vec;
    use std::vec::Vec;

    fn enabled() -> Schedule {
        Schedule {
            enabled: 1,
            ..Schedule::FACTORY_DEFAULT
        }
    }

    // 2026-10-24T00:00:00Z
    const T0_UNIX_MS: i64 = 1_792_800_000_000;

    struct Sim {
        planner: Planner,
        machine: Machine,
        now_ms: u64,
        unix_offset_ms: Option<i64>,
        fired_at: Vec<(u64, u32)>,
        healthy: bool,
    }

    impl Sim {
        fn new(schedule: Schedule, rtc: bool) -> Self {
            Self {
                planner: Planner::new(schedule, 0, 3),
                machine: Machine::boot(0, false),
                now_ms: 0,
                unix_offset_ms: rtc.then_some(T0_UNIX_MS),
                fired_at: Vec::new(),
                healthy: true,
            }
        }

        fn unix_ms(&self) -> Option<i64> {
            self.unix_offset_ms.map(|o| o + self.now_ms as i64)
        }

        /// One 40 ms frame: health, machine tick, planner actions.
        fn step(&mut self) {
            self.machine.update_health(Health {
                rtc_valid: self.unix_offset_ms.is_some(),
                pgood: true,
                adc_valid: self.healthy,
                measured_at_ms: self.now_ms,
            });
            if self.machine.state() == State::Pulsing {
                let _ = self.machine.pulse_finished(self.now_ms + 1);
            }
            self.machine.tick(self.now_ms);
            for _ in 0..3 {
                let action = self.planner.poll(Inputs {
                    now_ms: self.now_ms,
                    unix_ms: self.unix_ms(),
                    state: self.machine.state(),
                    machine_run_id: self.machine.run_id(),
                    hold: false,
                });
                let ok = match action {
                    Action::None => break,
                    Action::StartBaseline(req) => {
                        let ok = self.machine.start_baseline(req, self.now_ms).is_ok();
                        if !ok {
                            self.planner.start_rejected();
                        }
                        ok
                    }
                    Action::Arm { run_id, duration } => {
                        self.machine.arm(run_id, duration, self.now_ms).is_ok()
                    }
                    Action::Fire { run_id, nonce } => {
                        match self.machine.request_fire(run_id, nonce, self.now_ms) {
                            Ok(Effect::PersistLockout) => {
                                self.machine.confirm_lockout_persisted(self.now_ms).unwrap();
                                self.fired_at.push((self.now_ms, run_id));
                                true
                            }
                            _ => false,
                        }
                    }
                    Action::Abort { .. } => {
                        self.machine.abort(self.now_ms);
                        true
                    }
                };
                if !ok {
                    break;
                }
            }
        }

        fn run_for_ms(&mut self, ms: u64) {
            let end = self.now_ms + ms;
            while self.now_ms < end {
                self.step();
                self.now_ms += 40;
            }
        }
    }

    #[test]
    fn aligned_slots() {
        assert_eq!(next_aligned(0, 0, 3600), 0);
        assert_eq!(next_aligned(1, 0, 3600), 3600);
        assert_eq!(next_aligned(3600, 300, 3600), 3900);
        assert_eq!(next_aligned(3901, 300, 3600), 7500);
        assert_eq!(next_aligned(299, 300, 3600), 300);
        // Phase larger than the interval is just an offset modulo interval.
        assert_eq!(next_aligned(0, 4000, 3600), 400);
    }

    #[test]
    fn disabled_schedule_never_starts() {
        let mut sim = Sim::new(Schedule::FACTORY_DEFAULT, true);
        sim.run_for_ms(3 * 3_600_000);
        assert!(sim.fired_at.is_empty());
        assert_eq!(sim.planner.next_pulse_unix_s(sim.now_ms, sim.unix_ms()), 0);
    }

    #[test]
    fn pulses_on_rtc_slots() {
        let mut sim = Sim::new(enabled(), true);
        // T0 is aligned to 3600 s; the first usable slot is T0 + 3600 because
        // the T0 slot's baseline would have started 60 s before boot.
        assert_eq!(
            sim.planner.next_pulse_unix_s(0, sim.unix_ms()),
            (T0_UNIX_MS / 1000 + 3600) as u32
        );
        sim.run_for_ms(3 * 3_600_000 + 1_000);
        let fires: Vec<u64> = sim.fired_at.iter().map(|f| f.0).collect();
        assert_eq!(fires.len(), 3);
        for (i, at) in fires.iter().enumerate() {
            let slot = (i as u64 + 1) * 3_600_000;
            assert!(*at >= slot && *at < slot + 80, "fire {i} at {at}");
        }
        // Distinct scheduled run ids carrying the boot count.
        let ids: Vec<u32> = sim.fired_at.iter().map(|f| f.1).collect();
        assert_eq!(ids, vec![0x8003_0001, 0x8003_0002, 0x8003_0003]);
    }

    #[test]
    fn baseline_starts_baseline_s_early() {
        let mut sim = Sim::new(enabled(), true);
        sim.run_for_ms(3_600_000 - 60_000 - 40);
        assert_eq!(sim.machine.state(), State::SafeIdle);
        sim.run_for_ms(80);
        assert_eq!(sim.machine.state(), State::Baseline);
    }

    #[test]
    fn rtc_invalid_uses_monotonic_interval_from_anchor() {
        let mut sim = Sim::new(enabled(), false);
        sim.run_for_ms(2 * 3_600_000 + 1_000);
        assert_eq!(sim.fired_at.len(), 2);
        assert!(sim.fired_at[0].0 >= 3_600_000 && sim.fired_at[0].0 < 3_600_080);
        assert!(sim.fired_at[1].0 >= 7_200_000 && sim.fired_at[1].0 < 7_200_080);
        let run = sim.planner.active_run();
        assert!(run.is_none_or(|r| r.time_invalid));
        assert_eq!(sim.planner.next_pulse_unix_s(sim.now_ms, None), 0);
    }

    #[test]
    fn disabling_during_baseline_aborts_but_never_after_fire() {
        let mut sim = Sim::new(enabled(), true);
        sim.run_for_ms(3_600_000 - 30_000);
        assert_eq!(sim.machine.state(), State::Baseline);
        sim.planner
            .set_schedule(Schedule::FACTORY_DEFAULT, sim.now_ms);
        sim.run_for_ms(80);
        assert_eq!(sim.machine.state(), State::SafeIdle);
        sim.run_for_ms(3_600_000);
        assert!(sim.fired_at.is_empty());

        let mut sim = Sim::new(enabled(), true);
        sim.run_for_ms(3_600_000 + 1_000);
        assert_eq!(sim.fired_at.len(), 1);
        assert_eq!(sim.machine.state(), State::Recovery);
        sim.planner
            .set_schedule(Schedule::FACTORY_DEFAULT, sim.now_ms);
        sim.run_for_ms(80);
        assert_eq!(sim.machine.state(), State::Recovery);
    }

    #[test]
    fn unhealthy_run_is_aborted_after_grace_and_slot_skipped() {
        let mut sim = Sim::new(enabled(), true);
        sim.healthy = false;
        sim.run_for_ms(3_600_000 + FIRE_GRACE_MS + 200);
        assert!(sim.fired_at.is_empty());
        assert_eq!(sim.machine.state(), State::SafeIdle);
        sim.healthy = true;
        sim.run_for_ms(3_600_000);
        assert_eq!(sim.fired_at.len(), 1);
        assert!(sim.fired_at[0].0 >= 7_200_000);
    }

    #[test]
    fn busy_machine_skips_slot() {
        let mut sim = Sim::new(enabled(), true);
        // A host-commanded run occupies the machine at the slot's baseline
        // start, so the planner must skip that slot, not queue it.
        sim.run_for_ms(3_600_000 - 61_000);
        sim.machine
            .start_baseline(
                BaselineRequest {
                    run_id: 42,
                    baseline_duration_ms: 300_000,
                    recovery_duration_ms: 60_000,
                    sample_hz: 25,
                    require_rtc: true,
                },
                sim.now_ms,
            )
            .unwrap();
        sim.run_for_ms(120_000);
        assert!(sim.fired_at.is_empty());
        assert_eq!(sim.machine.run_id(), 42);
        sim.machine.abort(sim.now_ms);
        sim.run_for_ms(3_600_000);
        assert_eq!(sim.fired_at.len(), 1);
        assert!(sim.fired_at[0].0 >= 7_200_000);
    }

    #[test]
    fn lease_gates_pulses() {
        let schedule = Schedule {
            flags: sap_flux_protocol::SCHEDULE_FLAG_REQUIRE_LEASE,
            ..enabled()
        };
        let mut sim = Sim::new(schedule, true);
        // Lease ends between the first and second slot.
        sim.planner.set_lease((T0_UNIX_MS / 1000 + 5_000) as u32);
        sim.run_for_ms(3 * 3_600_000);
        assert_eq!(sim.fired_at.len(), 1);
        assert!(!sim.planner.lease_ok(sim.unix_ms()));
        assert_eq!(sim.planner.next_pulse_unix_s(sim.now_ms, sim.unix_ms()), 0);

        // No RTC: a required lease can never be valid.
        let mut sim = Sim::new(schedule, false);
        sim.planner.set_lease(u32::MAX);
        sim.run_for_ms(2 * 3_600_000);
        assert!(sim.fired_at.is_empty());
    }

    #[test]
    fn rtc_step_moves_pending_slot() {
        let mut sim = Sim::new(enabled(), true);
        sim.run_for_ms(1_000_000);
        // Board was 100 s fast; a correction moves the slot later on the
        // monotonic clock.
        sim.unix_offset_ms = sim.unix_offset_ms.map(|o| o - 100_000);
        sim.run_for_ms(3_000_000);
        assert_eq!(sim.fired_at.len(), 1);
        assert!(sim.fired_at[0].0 >= 3_700_000 && sim.fired_at[0].0 < 3_700_080);
    }

    #[test]
    fn interval_respects_cooldown() {
        // The shortest valid interval still leaves the machine idle in time.
        let schedule = Schedule {
            enabled: 1,
            interval_s: 1_000,
            baseline_s: 60,
            recovery_s: 60,
            ..Schedule::FACTORY_DEFAULT
        };
        assert!(schedule.validate(COOLDOWN_MS as u32).is_ok());
        let mut sim = Sim::new(schedule, true);
        sim.run_for_ms(5 * 1_000_000 + 1_000);
        assert_eq!(sim.fired_at.len(), 5);
    }
}
