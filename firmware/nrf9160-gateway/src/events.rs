//! Small queue of gateway events for the server (`0x03 Event`).
//!
//! Events are stamped with `Instant` when raised and converted to Unix ms
//! when sent, so events raised before the clock is set still get a real
//! time once it is. If the queue is full the oldest event is dropped.

use arrayvec::{ArrayString, ArrayVec};
use core::fmt::Write;
use embassy_time::Instant;

pub use sapflux_uplink::{
    EVENT_BOARD_LINK_LOST, EVENT_BOARD_LINK_RESTORED, EVENT_BOOT, EVENT_MODEM_ERROR,
    EVENT_RTC_STEP, EVENT_WINDOW_DROPPED,
};

/// Proposed extension (not in docs/field-protocol.md): the board rejected a
/// gateway command or a fetched window failed its CRC. The uplink codec
/// does not restrict codes.
pub const EVENT_GATEWAY_BOARD_ERROR: u16 = 7;

pub const QUEUE: usize = 8;
pub const TEXT: usize = 160;

#[derive(Clone)]
pub struct Event {
    pub at: Instant,
    pub code: u16,
    pub text: ArrayString<TEXT>,
}

pub struct Events {
    q: ArrayVec<Event, QUEUE>,
    pub dropped: u32,
}

impl Events {
    pub const fn new() -> Self {
        Self {
            q: ArrayVec::new_const(),
            dropped: 0,
        }
    }

    pub fn push(&mut self, code: u16, args: core::fmt::Arguments<'_>) {
        let mut text = ArrayString::<TEXT>::new();
        // Truncate silently at TEXT bytes (on a char boundary).
        let _ = TruncWriter(&mut text).write_fmt(args);
        crate::info!("event {}: {}", code, text.as_str());
        if self.q.is_full() {
            self.q.remove(0);
            self.dropped += 1;
        }
        self.q.push(Event {
            at: Instant::now(),
            code,
            text,
        });
    }

    pub fn front(&self) -> Option<&Event> {
        self.q.first()
    }

    pub fn pop_front(&mut self) {
        if !self.q.is_empty() {
            self.q.remove(0);
        }
    }
}

struct TruncWriter<'a>(&'a mut ArrayString<TEXT>);

impl Write for TruncWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if self.0.try_push(c).is_err() {
                break;
            }
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! event {
    ($events:expr, $code:expr, $($arg:tt)*) => {
        $events.push($code, format_args!($($arg)*))
    };
}
