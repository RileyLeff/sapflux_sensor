pub const COOLDOWN_MS: u64 = 600_000;
pub const HEALTH_MAX_AGE_MS: u64 = 500;

const MIN_RECOVERY_MS: u32 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    SafeIdle,
    Baseline,
    Armed,
    Pulsing,
    Recovery,
    Cooldown,
    Fault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PulseDuration {
    Ms500,
    Ms1000,
    Ms1500,
}

impl PulseDuration {
    pub const fn from_ms(value: u16) -> Option<Self> {
        match value {
            500 => Some(Self::Ms500),
            1_000 => Some(Self::Ms1000),
            1_500 => Some(Self::Ms1500),
            _ => None,
        }
    }

    pub const fn as_ms(self) -> u16 {
        match self {
            Self::Ms500 => 500,
            Self::Ms1000 => 1_000,
            Self::Ms1500 => 1_500,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BaselineRequest {
    pub run_id: u32,
    pub baseline_duration_ms: u32,
    pub recovery_duration_ms: u32,
    pub sample_hz: u16,
    /// Require a valid RTC to arm and fire this run. Host-commanded runs
    /// always set this. Autonomous scheduled runs clear it only while the
    /// RTC is invalid (the field protocol's `TIME_INVALID` mode); every other
    /// health check (PGOOD, ADC, staleness) and every lockout/cooldown rule
    /// still applies.
    pub require_rtc: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    WrongState,
    WrongRun,
    InvalidParameter,
    BaselineIncomplete,
    RtcInvalid,
    PowerNotGood,
    AdcUnhealthy,
    HealthStale,
    LockoutPending,
    DuplicateFire,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    None,
    PersistLockout,
    StartHardwarePulse(PulseDuration),
    ForceHeaterOff,
    ClearLockout,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Health {
    pub rtc_valid: bool,
    pub pgood: bool,
    pub adc_valid: bool,
    pub measured_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Machine {
    state: State,
    run_id: u32,
    pulse_duration: PulseDuration,
    baseline_ready_at_ms: u64,
    recovery_duration_ms: u32,
    recovery_ends_at_ms: u64,
    cooldown_ends_at_ms: u64,
    pulse_deadline_ms: u64,
    lockout_pending: bool,
    fire_nonce: Option<u64>,
    require_rtc: bool,
    health: Health,
}

impl Machine {
    pub fn boot(now_ms: u64, persistent_lockout: bool) -> Self {
        Self {
            state: if persistent_lockout {
                State::Cooldown
            } else {
                State::SafeIdle
            },
            run_id: 0,
            pulse_duration: PulseDuration::Ms500,
            baseline_ready_at_ms: 0,
            recovery_duration_ms: 180_000,
            recovery_ends_at_ms: 0,
            cooldown_ends_at_ms: if persistent_lockout {
                now_ms.saturating_add(COOLDOWN_MS)
            } else {
                0
            },
            pulse_deadline_ms: 0,
            lockout_pending: false,
            fire_nonce: None,
            require_rtc: true,
            health: Health::default(),
        }
    }

    pub const fn state(&self) -> State {
        self.state
    }

    pub const fn run_id(&self) -> u32 {
        self.run_id
    }

    pub const fn pulse_duration(&self) -> PulseDuration {
        self.pulse_duration
    }

    pub fn cooldown_remaining_ms(&self, now_ms: u64) -> u32 {
        self.cooldown_ends_at_ms
            .saturating_sub(now_ms)
            .min(u32::MAX as u64) as u32
    }

    pub fn update_health(&mut self, health: Health) {
        self.health = health;
    }

    pub fn start_baseline(
        &mut self,
        request: BaselineRequest,
        now_ms: u64,
    ) -> Result<Effect, Rejection> {
        if self.state != State::SafeIdle {
            return Err(Rejection::WrongState);
        }
        if request.run_id == 0
            || !(10_000..=300_000).contains(&request.baseline_duration_ms)
            || !(MIN_RECOVERY_MS..=600_000).contains(&request.recovery_duration_ms)
            || !(1..=100).contains(&request.sample_hz)
        {
            return Err(Rejection::InvalidParameter);
        }

        self.run_id = request.run_id;
        self.baseline_ready_at_ms = now_ms.saturating_add(request.baseline_duration_ms as u64);
        self.recovery_duration_ms = request.recovery_duration_ms;
        self.require_rtc = request.require_rtc;
        self.fire_nonce = None;
        self.lockout_pending = false;
        self.state = State::Baseline;
        Ok(Effect::None)
    }

    pub fn arm(
        &mut self,
        run_id: u32,
        duration: PulseDuration,
        now_ms: u64,
    ) -> Result<Effect, Rejection> {
        if self.state != State::Baseline {
            return Err(Rejection::WrongState);
        }
        if self.run_id != run_id {
            return Err(Rejection::WrongRun);
        }
        if now_ms < self.baseline_ready_at_ms {
            return Err(Rejection::BaselineIncomplete);
        }
        self.require_healthy(now_ms)?;

        self.pulse_duration = duration;
        self.state = State::Armed;
        Ok(Effect::None)
    }

    pub fn request_fire(
        &mut self,
        run_id: u32,
        command_nonce: u64,
        now_ms: u64,
    ) -> Result<Effect, Rejection> {
        if self.state != State::Armed {
            return Err(Rejection::WrongState);
        }
        if self.run_id != run_id {
            return Err(Rejection::WrongRun);
        }
        if self.fire_nonce == Some(command_nonce) {
            return Err(Rejection::DuplicateFire);
        }
        if self.lockout_pending {
            return Err(Rejection::LockoutPending);
        }
        self.require_healthy(now_ms)?;

        self.fire_nonce = Some(command_nonce);
        self.lockout_pending = true;
        self.cooldown_ends_at_ms = now_ms.saturating_add(COOLDOWN_MS);
        Ok(Effect::PersistLockout)
    }

    /// Call only after the EEPROM lockout record was written and read back.
    pub fn confirm_lockout_persisted(&mut self, now_ms: u64) -> Result<Effect, Rejection> {
        if self.state != State::Armed || !self.lockout_pending {
            return Err(Rejection::WrongState);
        }
        self.lockout_pending = false;
        self.pulse_deadline_ms = now_ms.saturating_add(self.pulse_duration.as_ms() as u64);
        self.state = State::Pulsing;
        Ok(Effect::StartHardwarePulse(self.pulse_duration))
    }

    pub fn persistence_failed(&mut self) -> Effect {
        self.lockout_pending = false;
        self.state = State::Fault;
        Effect::ForceHeaterOff
    }

    /// Allowed lateness of the hardware pulse end before declaring a fault.
    /// The pulse is timed by TIM1 on HSI16 (up to ~2.75% slow over
    /// temperature) while `now_ms` runs on the LSE crystal, and pulse end is
    /// only seen between ADC acquisitions (~25 ms), so allow 5% + 40 ms.
    fn overrun_tolerance_ms(&self) -> u64 {
        let pulse_ms = self.pulse_duration.as_ms() as u64;
        pulse_ms / 20 + 40
    }

    pub fn pulse_finished(&mut self, now_ms: u64) -> Result<Effect, Rejection> {
        if self.state != State::Pulsing {
            return Err(Rejection::WrongState);
        }
        self.recovery_ends_at_ms = now_ms.saturating_add(self.recovery_duration_ms as u64);
        self.state = State::Recovery;
        Ok(Effect::ForceHeaterOff)
    }

    pub fn abort(&mut self, now_ms: u64) -> Effect {
        match self.state {
            State::Baseline | State::Armed if !self.lockout_pending => {
                self.state = State::SafeIdle;
                self.run_id = 0;
                Effect::ForceHeaterOff
            }
            State::Armed | State::Pulsing | State::Recovery => {
                self.lockout_pending = false;
                self.recovery_ends_at_ms = now_ms.saturating_add(self.recovery_duration_ms as u64);
                self.state = State::Recovery;
                Effect::ForceHeaterOff
            }
            _ => Effect::ForceHeaterOff,
        }
    }

    pub fn tick(&mut self, now_ms: u64) -> Effect {
        match self.state {
            State::Pulsing
                if now_ms
                    > self
                        .pulse_deadline_ms
                        .saturating_add(self.overrun_tolerance_ms()) =>
            {
                self.state = State::Fault;
                Effect::ForceHeaterOff
            }
            State::Recovery if now_ms >= self.recovery_ends_at_ms => {
                if now_ms >= self.cooldown_ends_at_ms {
                    self.state = State::SafeIdle;
                    self.run_id = 0;
                    Effect::ClearLockout
                } else {
                    self.state = State::Cooldown;
                    Effect::ForceHeaterOff
                }
            }
            State::Cooldown if now_ms >= self.cooldown_ends_at_ms => {
                self.state = State::SafeIdle;
                self.run_id = 0;
                Effect::ClearLockout
            }
            _ => Effect::None,
        }
    }

    fn require_healthy(&self, now_ms: u64) -> Result<(), Rejection> {
        if self.require_rtc && !self.health.rtc_valid {
            return Err(Rejection::RtcInvalid);
        }
        if !self.health.pgood {
            return Err(Rejection::PowerNotGood);
        }
        if !self.health.adc_valid {
            return Err(Rejection::AdcUnhealthy);
        }
        if now_ms.saturating_sub(self.health.measured_at_ms) > HEALTH_MAX_AGE_MS {
            return Err(Rejection::HealthStale);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy(at: u64) -> Health {
        Health {
            rtc_valid: true,
            pgood: true,
            adc_valid: true,
            measured_at_ms: at,
        }
    }

    fn baseline(machine: &mut Machine, now: u64) {
        machine
            .start_baseline(
                BaselineRequest {
                    run_id: 7,
                    baseline_duration_ms: 60_000,
                    recovery_duration_ms: 180_000,
                    sample_hz: 50,
                    require_rtc: true,
                },
                now,
            )
            .unwrap();
    }

    #[test]
    fn boot_lockout_requires_full_powered_cooldown() {
        let mut machine = Machine::boot(1_000, true);
        assert_eq!(machine.state(), State::Cooldown);
        assert_eq!(machine.cooldown_remaining_ms(1_000), COOLDOWN_MS as u32);
        assert_eq!(machine.tick(1_000 + COOLDOWN_MS - 1), Effect::None);
        assert_eq!(machine.tick(1_000 + COOLDOWN_MS), Effect::ClearLockout);
        assert_eq!(machine.state(), State::SafeIdle);
    }

    #[test]
    fn baseline_and_health_are_required_to_arm() {
        let mut machine = Machine::boot(0, false);
        baseline(&mut machine, 0);
        machine.update_health(healthy(59_900));
        assert_eq!(
            machine.arm(7, PulseDuration::Ms500, 59_999),
            Err(Rejection::BaselineIncomplete)
        );
        machine.update_health(Health {
            pgood: false,
            ..healthy(60_000)
        });
        assert_eq!(
            machine.arm(7, PulseDuration::Ms500, 60_000),
            Err(Rejection::PowerNotGood)
        );
        machine.update_health(healthy(60_000));
        assert_eq!(
            machine.arm(7, PulseDuration::Ms500, 60_000),
            Ok(Effect::None)
        );
    }

    #[test]
    fn fire_cannot_start_until_lockout_is_persisted() {
        let mut machine = Machine::boot(0, false);
        baseline(&mut machine, 0);
        machine.update_health(healthy(60_000));
        machine.arm(7, PulseDuration::Ms500, 60_000).unwrap();

        assert_eq!(
            machine.request_fire(7, 123, 60_000),
            Ok(Effect::PersistLockout)
        );
        assert_eq!(machine.state(), State::Armed);
        assert_eq!(
            machine.request_fire(7, 123, 60_001),
            Err(Rejection::DuplicateFire)
        );
        assert_eq!(
            machine.request_fire(7, 124, 60_001),
            Err(Rejection::LockoutPending)
        );
        assert_eq!(
            machine.confirm_lockout_persisted(60_002),
            Ok(Effect::StartHardwarePulse(PulseDuration::Ms500))
        );
        assert_eq!(machine.state(), State::Pulsing);
    }

    #[test]
    fn persistence_failure_never_enters_pulsing() {
        let mut machine = Machine::boot(0, false);
        baseline(&mut machine, 0);
        machine.update_health(healthy(60_000));
        machine.arm(7, PulseDuration::Ms500, 60_000).unwrap();
        machine.request_fire(7, 123, 60_000).unwrap();
        assert_eq!(machine.persistence_failed(), Effect::ForceHeaterOff);
        assert_eq!(machine.state(), State::Fault);
    }

    #[test]
    fn only_three_pulse_lengths_exist() {
        assert_eq!(PulseDuration::from_ms(499), None);
        assert_eq!(PulseDuration::from_ms(500), Some(PulseDuration::Ms500));
        assert_eq!(PulseDuration::from_ms(1_000), Some(PulseDuration::Ms1000));
        assert_eq!(PulseDuration::from_ms(1_500), Some(PulseDuration::Ms1500));
        assert_eq!(PulseDuration::from_ms(1_501), None);
    }

    #[test]
    fn recovery_and_cooldown_both_finish_before_lockout_clears() {
        let mut machine = Machine::boot(0, false);
        machine
            .start_baseline(
                BaselineRequest {
                    run_id: 7,
                    baseline_duration_ms: 60_000,
                    recovery_duration_ms: 60_000,
                    sample_hz: 50,
                    require_rtc: true,
                },
                0,
            )
            .unwrap();
        machine.update_health(healthy(60_000));
        machine.arm(7, PulseDuration::Ms500, 60_000).unwrap();
        machine.request_fire(7, 123, 60_000).unwrap();
        machine.confirm_lockout_persisted(60_002).unwrap();
        machine.pulse_finished(60_502).unwrap();

        let recovery_end = 120_502;
        let cooldown_end = 60_000 + COOLDOWN_MS;
        assert_eq!(machine.tick(recovery_end - 1), Effect::None);
        if recovery_end >= cooldown_end {
            assert_eq!(machine.tick(recovery_end), Effect::ClearLockout);
        } else {
            assert_eq!(machine.tick(recovery_end), Effect::ForceHeaterOff);
            assert_eq!(machine.state(), State::Cooldown);
            assert_eq!(machine.tick(cooldown_end), Effect::ClearLockout);
        }
        assert_eq!(machine.state(), State::SafeIdle);
    }

    #[test]
    fn rtc_is_required_unless_the_run_opts_out() {
        let request = BaselineRequest {
            run_id: 7,
            baseline_duration_ms: 60_000,
            recovery_duration_ms: 180_000,
            sample_hz: 25,
            require_rtc: true,
        };
        let no_rtc = Health {
            rtc_valid: false,
            ..healthy(60_000)
        };

        let mut machine = Machine::boot(0, false);
        machine.start_baseline(request, 0).unwrap();
        machine.update_health(no_rtc);
        assert_eq!(
            machine.arm(7, PulseDuration::Ms1000, 60_000),
            Err(Rejection::RtcInvalid)
        );

        let mut machine = Machine::boot(0, false);
        machine
            .start_baseline(
                BaselineRequest {
                    require_rtc: false,
                    ..request
                },
                0,
            )
            .unwrap();
        machine.update_health(Health {
            adc_valid: false,
            ..no_rtc
        });
        // Opting out of the RTC never relaxes the other health checks.
        assert_eq!(
            machine.arm(7, PulseDuration::Ms1000, 60_000),
            Err(Rejection::AdcUnhealthy)
        );
        machine.update_health(no_rtc);
        assert_eq!(
            machine.arm(7, PulseDuration::Ms1000, 60_000),
            Ok(Effect::None)
        );
        assert_eq!(
            machine.request_fire(7, 1, 60_000),
            Ok(Effect::PersistLockout)
        );

        // The next run starts strict again.
        let mut machine = Machine::boot(0, false);
        machine.start_baseline(request, 0).unwrap();
        machine.update_health(no_rtc);
        assert_eq!(
            machine.arm(7, PulseDuration::Ms1000, 60_000),
            Err(Rejection::RtcInvalid)
        );
    }

    #[test]
    fn missed_hardware_completion_forces_fault() {
        let mut machine = Machine::boot(0, false);
        baseline(&mut machine, 0);
        machine.update_health(healthy(60_000));
        machine.arm(7, PulseDuration::Ms500, 60_000).unwrap();
        machine.request_fire(7, 123, 60_000).unwrap();
        machine.confirm_lockout_persisted(60_000).unwrap();
        // 500 ms pulse: tolerance is 500 / 20 + 40 = 65 ms.
        assert_eq!(machine.tick(60_565), Effect::None);
        assert_eq!(machine.tick(60_566), Effect::ForceHeaterOff);
        assert_eq!(machine.state(), State::Fault);
    }

    #[test]
    fn slow_hsi_pulse_end_is_not_a_fault() {
        // TIM1 runs on HSI16, which can be ~2.75% slow when cold, so a
        // 1500 ms pulse can end ~1542 ms after it started on the LSE clock.
        let mut machine = Machine::boot(0, false);
        baseline(&mut machine, 0);
        machine.update_health(healthy(60_000));
        machine.arm(7, PulseDuration::Ms1500, 60_000).unwrap();
        machine.request_fire(7, 123, 60_000).unwrap();
        machine.confirm_lockout_persisted(60_000).unwrap();
        assert_eq!(machine.tick(61_543), Effect::None);
        assert_eq!(machine.pulse_finished(61_543), Ok(Effect::ForceHeaterOff));
        assert_eq!(machine.state(), State::Recovery);
    }
}
