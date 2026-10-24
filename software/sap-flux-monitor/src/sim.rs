//! Simulated board.
//!
//! Produces the same COBS byte stream a real board would, so the worker
//! feeds it through the identical decoder → logger → UI path. It never
//! opens a serial port and never causes the app to send commands; it only
//! *responds* to commands the user explicitly sends.
//!
//! Behavior: Hello on start, samples at 10 Hz idle (requested rate during a
//! run), Status at 1 Hz, RTC becomes valid on its own after ~12 s (or
//! immediately on SetRtc), all six thermistor channels are healthy, and a
//! fired pulse produces a plausible heat/decay trace followed by recovery and a
//! 10-minute board-reported cooldown. Optionally injects CRC errors and
//! sequence gaps for exercising the error counters.

use sap_flux_protocol::{
    encode, Ack, BoardState, Hello, Message, Sample, Status, CAP_ADC, CAP_HEATER_CONTROL, CAP_RTC,
    CAP_V24_MONITOR, FLAG_EXCITATION_ACTIVE, FLAG_HEATER_ACTIVE, FLAG_PGOOD, FLAG_RTC_VALID,
    MAX_ENCODED_LEN,
};
use std::time::{Duration, Instant};

const IDLE_SAMPLE_HZ: f64 = 10.0;
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
const RTC_AUTO_VALID_AFTER: Duration = Duration::from_secs(12);
const COOLDOWN: Duration = Duration::from_secs(600);
/// Inject one corrupted frame every N samples when enabled.
const CORRUPT_EVERY: u64 = 120;
/// Skip one sequence number every N samples when enabled.
const GAP_EVERY: u64 = 197;

/// Deterministic xorshift PRNG for sample noise.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in [-1, 1).
    fn noise(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 52) as f64 * 2.0 - 1.0
    }
}

/// A simulated sap-flux board.
pub struct SimBoard {
    start: Instant,
    rng: Rng,
    seq: u32,
    sample_count: u64,
    next_sample_at: Instant,
    next_status_at: Instant,
    hello_sent: bool,
    inject_errors: bool,

    state: BoardState,
    run_id: u32,
    pulse_ms: u16,
    sample_hz: f64,
    rtc_valid: bool,
    rtc_base_ms: i64,
    pending_recovery_ms: u32,
    pulse_started: Option<Instant>,
    recovery_ends: Option<Instant>,
    cooldown_ends: Option<Instant>,
    /// Time of the most recent fired pulse, for the thermal trace.
    last_fire: Option<Instant>,
    last_fire_pulse_s: f64,
}

impl SimBoard {
    /// Create a simulator; `inject_errors` adds periodic CRC corruption and
    /// sequence gaps to the produced stream.
    pub fn new(now: Instant, inject_errors: bool) -> Self {
        Self {
            start: now,
            rng: Rng(0x51DB_0A2D_5EED),
            seq: 0,
            sample_count: 0,
            next_sample_at: now,
            next_status_at: now,
            hello_sent: false,
            inject_errors,
            state: BoardState::SafeIdle,
            run_id: 0,
            pulse_ms: 0,
            sample_hz: IDLE_SAMPLE_HZ,
            rtc_valid: false,
            rtc_base_ms: crate::types::host_unix_ns() / 1_000_000,
            pending_recovery_ms: 180_000,
            pulse_started: None,
            recovery_ends: None,
            cooldown_ends: None,
            last_fire: None,
            last_fire_pulse_s: 0.5,
        }
    }

    fn monotonic_us(&self, now: Instant) -> u64 {
        now.duration_since(self.start).as_micros() as u64
    }

    fn rtc_ms(&self, now: Instant) -> i64 {
        if self.rtc_valid {
            self.rtc_base_ms + now.duration_since(self.start).as_millis() as i64
        } else {
            -1
        }
    }

    fn cooldown_remaining_ms(&self, now: Instant) -> u32 {
        match self.cooldown_ends {
            Some(t) if t > now => t.duration_since(now).as_millis() as u32,
            _ => 0,
        }
    }

    fn push_frame(&mut self, msg: &Message, out: &mut Vec<u8>, corrupt: bool) {
        let mut buf = [0u8; MAX_ENCODED_LEN];
        let n = encode(msg, self.seq, &mut buf).expect("sim frames always fit");
        self.seq = self.seq.wrapping_add(1);
        if corrupt {
            // Flip a bit mid-frame; the decoder must report a CRC or
            // framing error and resynchronize.
            buf[n / 2] ^= 0x10;
        }
        out.extend_from_slice(&buf[..n]);
    }

    /// Advance the state machine and append any due frames to `out`.
    pub fn poll(&mut self, now: Instant, out: &mut Vec<u8>) {
        if !self.hello_sent {
            self.hello_sent = true;
            // Full capability set — including heater control — so the
            // complete safety UX stays testable without hardware. Command
            // handling is immediate and never depends on command windows.
            let hello = Message::Hello(Hello {
                board_id: 0x51D0_0000_0000_CAFE,
                firmware_version: 1 << 8,
                capabilities: CAP_RTC | CAP_ADC | CAP_V24_MONITOR | CAP_HEATER_CONTROL,
            });
            self.push_frame(&hello, out, false);
        }

        if !self.rtc_valid && now.duration_since(self.start) >= RTC_AUTO_VALID_AFTER {
            self.rtc_valid = true;
        }

        // Pulse -> Recovery -> Cooldown -> SafeIdle transitions.
        if self.state == BoardState::Pulsing {
            if let Some(t0) = self.pulse_started {
                if now.duration_since(t0) >= Duration::from_millis(self.pulse_ms as u64) {
                    self.state = BoardState::Recovery;
                    self.pulse_started = None;
                }
            }
        }
        if self.state == BoardState::Recovery {
            if let Some(t) = self.recovery_ends {
                if now >= t {
                    self.state = BoardState::Cooldown;
                    self.recovery_ends = None;
                    self.sample_hz = IDLE_SAMPLE_HZ;
                }
            }
        }
        if self.state == BoardState::Cooldown && self.cooldown_remaining_ms(now) == 0 {
            self.state = BoardState::SafeIdle;
            self.run_id = 0;
        }

        while now >= self.next_sample_at {
            let interval = Duration::from_secs_f64(1.0 / self.sample_hz);
            self.next_sample_at += interval;
            self.emit_sample(now, out);
        }

        if now >= self.next_status_at {
            self.next_status_at = now + STATUS_INTERVAL;
            let status = Message::Status(Status {
                monotonic_us: self.monotonic_us(now),
                rtc_unix_ms: self.rtc_ms(now),
                run_id: self.run_id,
                state: self.state,
                fault_code: 0,
                pulse_duration_ms: self.pulse_ms,
                cooldown_remaining_ms: self.cooldown_remaining_ms(now),
            });
            self.push_frame(&status, out, false);
        }
    }

    fn emit_sample(&mut self, now: Instant, out: &mut Vec<u8>) {
        self.sample_count += 1;
        let t = now.duration_since(self.start).as_secs_f64();
        let heater = self.state == BoardState::Pulsing;

        // Excitation ~3.2 V on a ±4.096 V ADS1115 scale, with a little noise.
        let exc = 26000.0 + 40.0 * (t / 47.0).sin() + self.rng.noise() * 6.0;

        // Heat-pulse response: exponential rise/decay after a fire.
        let pulse_heat = |base_amp: f64, rng_noise: f64, fired: Option<Instant>| -> f64 {
            match fired {
                Some(t0) => {
                    let dt = now.duration_since(t0).as_secs_f64();
                    let amp = base_amp * (self.last_fire_pulse_s / 0.5);
                    // Fast rise while heating + slow conductive decay.
                    let rise = 1.0 - (-dt / 0.8).exp();
                    let decay = (-dt / 45.0).exp();
                    amp * rise * decay + rng_noise
                }
                None => rng_noise,
            }
        };

        let mut temps = [0.0f64; 6];
        // Base temperatures with a slow drift; downstream slightly offset
        // from upstream. Amplitudes: middle sensors see the most heat.
        let bases = [24.1, 24.4, 24.2, 23.9, 24.2, 24.0];
        let amps = [1.1, 1.8, 1.5, 0.9, 1.5, 1.2];
        for i in 0..6 {
            let drift = 0.15 * (t / 60.0 + i as f64).sin();
            let noise = self.rng.noise() * 0.02;
            temps[i] = bases[i] + drift + pulse_heat(amps[i], noise, self.last_fire);
        }

        let mut raw = [0i16; 6];
        for i in 0..6 {
            // Invert via a beta-model NTC (close enough to the cubic for
            // plausible-looking data).
            let t_k = temps[i] + 273.15;
            let r = 10_000.0 * (3892.0 * (1.0 / t_k - 1.0 / 298.15)).exp();
            raw[i] = (exc * 10_000.0 / (10_000.0 + r)) as i16;
        }

        let mut flags = FLAG_PGOOD | FLAG_EXCITATION_ACTIVE;
        if self.rtc_valid {
            flags |= FLAG_RTC_VALID;
        }
        if heater {
            flags |= FLAG_HEATER_ACTIVE;
        }

        let msg = Message::Sample(Sample {
            monotonic_us: self.monotonic_us(now),
            rtc_unix_ms: self.rtc_ms(now),
            run_id: self.run_id,
            flags,
            excitation_raw: exc as i16,
            thermistor_raw: raw,
            v24_raw: (24_000.0 + self.rng.noise() * 30.0) as i16,
            state: self.state,
        });

        let corrupt = self.inject_errors && self.sample_count.is_multiple_of(CORRUPT_EVERY);
        if self.inject_errors && self.sample_count.is_multiple_of(GAP_EVERY) {
            self.seq = self.seq.wrapping_add(1); // manufactured sequence gap
        }
        self.push_frame(&msg, out, corrupt);
    }

    /// Handle a host command exactly as the firmware state machine would,
    /// appending the Ack (and any follow-up frames) to `out`.
    pub fn handle_command(&mut self, msg: &Message, now: Instant, out: &mut Vec<u8>) {
        let (command_type, nonce, run_id, result) = match *msg {
            Message::Query(q) => {
                self.hello_sent = false; // re-emit Hello on next poll
                self.next_status_at = now;
                (msg.message_type(), q.command_nonce, self.run_id, 0)
            }
            Message::SetRtc(c) => {
                self.rtc_valid = true;
                self.rtc_base_ms = c.unix_ms - now.duration_since(self.start).as_millis() as i64;
                (msg.message_type(), c.command_nonce, self.run_id, 0)
            }
            Message::StartBaseline(c) => {
                let ok = self.state == BoardState::SafeIdle && self.cooldown_remaining_ms(now) == 0;
                if ok {
                    self.state = BoardState::Baseline;
                    self.run_id = c.run_id;
                    self.sample_hz = f64::from(c.requested_sample_hz.clamp(1, 100));
                    self.recovery_ends = None;
                    // Remember recovery duration for after the pulse.
                    self.pending_recovery_ms = c.recovery_duration_ms;
                }
                (msg.message_type(), c.command_nonce, c.run_id, u8::from(!ok))
            }
            Message::ArmPulse(c) => {
                let ok = self.state == BoardState::Baseline
                    && c.run_id == self.run_id
                    && self.rtc_valid
                    && matches!(c.pulse_duration_ms, 500 | 1000 | 1500);
                if ok {
                    self.state = BoardState::Armed;
                    self.pulse_ms = c.pulse_duration_ms;
                }
                (msg.message_type(), c.command_nonce, c.run_id, u8::from(!ok))
            }
            Message::FirePulse(c) => {
                let ok = self.state == BoardState::Armed && c.run_id == self.run_id;
                if ok {
                    self.state = BoardState::Pulsing;
                    self.pulse_started = Some(now);
                    self.last_fire = Some(now);
                    self.last_fire_pulse_s = f64::from(self.pulse_ms) / 1000.0;
                    self.recovery_ends = Some(
                        now + Duration::from_millis(self.pulse_ms as u64)
                            + Duration::from_millis(self.pending_recovery_ms as u64),
                    );
                    self.cooldown_ends = Some(now + COOLDOWN);
                }
                (msg.message_type(), c.command_nonce, c.run_id, u8::from(!ok))
            }
            Message::Abort(c) => {
                let active = matches!(
                    self.state,
                    BoardState::Baseline
                        | BoardState::Armed
                        | BoardState::Pulsing
                        | BoardState::Recovery
                );
                if active {
                    // A pulse already delivered still requires cooldown.
                    self.state = if self.cooldown_remaining_ms(now) > 0 {
                        BoardState::Cooldown
                    } else {
                        BoardState::SafeIdle
                    };
                    self.sample_hz = IDLE_SAMPLE_HZ;
                    self.pulse_started = None;
                    self.recovery_ends = None;
                }
                (
                    msg.message_type(),
                    c.command_nonce,
                    c.run_id,
                    u8::from(!active),
                )
            }
            // Board-to-host messages arriving here would be a host bug;
            // reject them like unknown commands.
            _ => (msg.message_type(), 0, self.run_id, 0xFF),
        };
        let ack = Message::Ack(Ack {
            command_type,
            result,
            command_nonce: nonce,
            run_id,
        });
        self.push_frame(&ack, out, false);
        // Status right after any state-changing command.
        self.next_status_at = now;
    }
}
