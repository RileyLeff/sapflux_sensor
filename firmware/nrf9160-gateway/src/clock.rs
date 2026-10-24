//! Gateway clock: Unix time = `base_ms + Instant::now()`.
//!
//! Sources, best first: four-timestamp exchange with the server (lowest
//! round trip of the cycle, round trip < 3 s), then network time from the
//! modem (`AT+CCLK?`, NITZ). Until one of them succeeds the clock is
//! invalid and timestamps are plain uptime milliseconds.

use embassy_time::Instant;

/// Exchanges with a round trip at or above this are discarded (spec).
pub const MAX_RTT_MS: i64 = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    None,
    Network,
    Server,
}

pub struct Clock {
    base_ms: i64,
    pub source: Source,
    /// Lowest round trip accepted in the current cycle.
    cycle_best_rtt: Option<i64>,
    pub last_server_sync: Option<Instant>,
}

impl Clock {
    pub const fn new() -> Self {
        Self {
            base_ms: 0,
            source: Source::None,
            cycle_best_rtt: None,
            last_server_sync: None,
        }
    }

    pub fn valid(&self) -> bool {
        self.source != Source::None
    }

    /// Gateway clock at `t` (uptime ms while invalid).
    pub fn at(&self, t: Instant) -> i64 {
        self.base_ms + t.as_millis() as i64
    }

    pub fn now(&self) -> i64 {
        self.at(Instant::now())
    }

    /// Unix ms at `t` if the clock is valid.
    pub fn unix_at(&self, t: Instant) -> Option<i64> {
        self.valid().then(|| self.at(t))
    }

    pub fn start_cycle(&mut self) {
        self.cycle_best_rtt = None;
    }

    /// True if a server sample was accepted this cycle.
    pub fn synced_this_cycle(&self) -> bool {
        self.cycle_best_rtt.is_some()
    }

    /// Feed a server exchange; `offset`/`rtt` from
    /// `CheckInReply::offset_rtt(t4)` where t1/t4 were read from this clock.
    /// Returns true if it was applied.
    pub fn server_sample(&mut self, offset_ms: i64, rtt_ms: i64) -> bool {
        if !(0..MAX_RTT_MS).contains(&rtt_ms) {
            return false;
        }
        if self.cycle_best_rtt.is_some_and(|best| rtt_ms >= best) {
            return false;
        }
        self.cycle_best_rtt = Some(rtt_ms);
        self.base_ms += offset_ms;
        self.source = Source::Server;
        self.last_server_sync = Some(Instant::now());
        true
    }

    /// Network time fallback; only used if no server sync ever happened (a
    /// stale server sync drifts only ~20 ppm, NITZ is often seconds off).
    pub fn network_sample(&mut self, unix_ms: i64, at: Instant) -> bool {
        if self.source == Source::Server {
            return false;
        }
        self.base_ms = unix_ms - at.as_millis() as i64;
        self.source = Source::Network;
        true
    }
}
