//! Line logger on the CP2102N USB UART (UARTE0 TX = P0.06, 1 Mbaud).
//!
//! The port is TX-only and can be released between cycles so the pins go
//! back to disconnected inputs: on battery the CP2102N is unpowered, and a
//! TX pin idling high would back-power it.

use core::cell::RefCell;
use core::fmt::{self, Write};

use critical_section::Mutex;
use embassy_nrf::peripherals;
use embassy_nrf::uarte::{self, Baudrate, UarteTx};
use embassy_time::Instant;

static PORT: Mutex<RefCell<Option<UarteTx<'static>>>> = Mutex::new(RefCell::new(None));

/// Claim UARTE0 for logging (idempotent).
pub fn enable() {
    critical_section::with(|cs| {
        let mut port = PORT.borrow_ref_mut(cs);
        if port.is_none() {
            let mut config = uarte::Config::default();
            config.baudrate = Baudrate::Baud1m;
            // SAFETY: SERIAL0 and P0.06 are reserved for this logger; main
            // never uses those fields of `embassy_nrf::Peripherals`, and only
            // one UarteTx exists at a time (guarded by PORT).
            let tx = unsafe {
                UarteTx::new(
                    peripherals::SERIAL0::steal(),
                    crate::Irqs,
                    peripherals::P0_06::steal(),
                    config,
                )
            };
            *port = Some(tx);
        }
    });
}

/// Release UARTE0 (disables the peripheral and disconnects P0.06) unless
/// the `log-always-on` feature is set.
pub fn disable() {
    if cfg!(feature = "log-always-on") {
        return;
    }
    let port = critical_section::with(|cs| PORT.borrow_ref_mut(cs).take());
    drop(port);
}

fn write_bytes(bytes: &[u8]) {
    // Take the port out so the blocking DMA write runs outside the critical
    // section (the modem IPC interrupt must not be held off).
    let port = critical_section::with(|cs| PORT.borrow_ref_mut(cs).take());
    if let Some(mut tx) = port {
        let _ = tx.blocking_write(bytes);
        critical_section::with(|cs| {
            let mut slot = PORT.borrow_ref_mut(cs);
            if slot.is_none() {
                *slot = Some(tx);
            }
        });
    }
}

/// Fixed buffer that silently truncates.
pub struct Line {
    buf: [u8; 320],
    len: usize,
}

impl Line {
    pub const fn new() -> Self {
        Self {
            buf: [0; 320],
            len: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn as_str(&self) -> &str {
        // Only ever filled through fmt::Write (UTF-8) but truncation may
        // split a code point; fall back to the valid prefix.
        match core::str::from_utf8(self.as_bytes()) {
            Ok(s) => s,
            Err(e) => core::str::from_utf8(&self.buf[..e.valid_up_to()]).unwrap_or(""),
        }
    }
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = self.buf.len() - self.len;
        let n = s.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Format and write one log line with an uptime prefix.
pub fn log_fmt(args: fmt::Arguments<'_>) {
    let mut line = Line::new();
    let ms = Instant::now().as_millis();
    let _ = write!(line, "[{:>6}.{:03}] ", ms / 1000, ms % 1000);
    let _ = line.write_fmt(args);
    if line.len > line.buf.len() - 2 {
        line.len = line.buf.len() - 2;
    }
    let _ = line.write_str("\r\n");
    write_bytes(line.as_bytes());
}

/// `info!("fmt", args...)`: one line on the USB UART.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::log::log_fmt(format_args!($($arg)*))
    };
}
