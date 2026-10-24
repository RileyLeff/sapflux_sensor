//! [`Transport`] over a real serial port.
//!
//! Two physical setups are supported:
//!
//! - **J6 (LPUART, field link)** through a 3.3 V USB-serial adapter:
//!   adapter TX → J6 p2 (board RX), adapter RX ← J6 p3 (board TX), GND → J6
//!   p4. Full duplex; RTS is not touched.
//! - **RS-485 (J5)** through the bench FT232R adapter, whose driver is
//!   enabled while RTS is high. With `rs485 = true`, RTS is held low except
//!   while a frame is being written.

use crate::link::Transport;
use serialport::{ClearBuffer, SerialPort};
use std::io::{self, Read, Write};
use std::time::Duration;

/// Default line rate (115200 8N1).
pub const DEFAULT_BAUD: u32 = 115_200;
/// Read poll interval.
const POLL: Duration = Duration::from_millis(5);
/// FT232R driver-enable settle before the first byte.
const RTS_SETTLE: Duration = Duration::from_micros(200);

/// A serial port as a link transport.
pub struct SerialTransport {
    port: Box<dyn SerialPort>,
    rs485: bool,
    baud: u32,
}

impl SerialTransport {
    /// Open `path` at `baud`, 8N1, no flow control.
    pub fn open(path: &str, baud: u32, rs485: bool) -> io::Result<Self> {
        let mut port = serialport::new(path, baud)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(POLL)
            .open()
            .map_err(io::Error::other)?;
        if rs485 {
            // Receive mode immediately so opening never jams the bus.
            port.write_request_to_send(false)
                .map_err(io::Error::other)?;
        }
        Ok(Self { port, rs485, baud })
    }

    /// Wrap an already-open port (e.g. one end of a pty pair in tests).
    pub fn from_port(port: Box<dyn SerialPort>, baud: u32, rs485: bool) -> Self {
        Self { port, rs485, baud }
    }

    /// Time the line needs to shift out `n` bytes (10 bits per byte).
    fn line_time(&self, n: usize) -> Duration {
        Duration::from_micros((n as u64 * 10 * 1_000_000).div_ceil(self.baud as u64))
    }
}

impl Transport for SerialTransport {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.rs485 {
            self.port
                .write_request_to_send(true)
                .map_err(io::Error::other)?;
            std::thread::sleep(RTS_SETTLE);
        }
        let result = self.port.write_all(bytes).and_then(|_| self.port.flush());
        if self.rs485 {
            // flush() drains the OS buffer; give the adapter's FIFO the
            // line time of the frame before turning the driver off.
            std::thread::sleep(self.line_time(bytes.len()));
            let _ = self.port.write_request_to_send(false);
        }
        result
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.port
            .clear(ClearBuffer::Input)
            .map_err(io::Error::other)
    }
}

/// Serial ports likely to be a USB-serial adapter (macOS `cu.*` callout
/// devices first).
pub fn candidate_ports() -> Vec<String> {
    let mut names: Vec<String> = serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();
    names.retain(|n| {
        !n.starts_with("/dev/tty.") && !n.contains("Bluetooth") && !n.contains("debug-console")
    });
    names.sort();
    names
}
