//! Exercise the real serial transport over a pseudo-terminal pair: the
//! test thread plays the board on the master side.

#![cfg(unix)]

use sap_flux_field_cli::link::{Link, LinkOptions};
use sap_flux_field_cli::serial::SerialTransport;
use sap_flux_protocol::*;
use serialport::{SerialPort, TTYPort};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[test]
fn field_status_over_pty_with_wake_byte_and_retry() {
    let (mut board, host) = TTYPort::pair().expect("pty pair");
    board.set_timeout(Duration::from_millis(5)).unwrap();
    let mut link = Link::new(
        SerialTransport::from_port(Box::new(host), 115_200, false),
        LinkOptions {
            timeout: Duration::from_millis(150),
            ..LinkOptions::default()
        },
    );

    let worker = std::thread::spawn(move || {
        let mut dec = Decoder::new();
        let mut wakes = 0;
        let mut zero_run = true; // a 0x00 with nothing before it is a wake byte
        let mut requests = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 256];
        while Instant::now() < deadline && requests < 2 {
            let n = match board.read(&mut buf) {
                Ok(n) => n,
                Err(_) => continue,
            };
            for &b in &buf[..n] {
                if b == 0 && zero_run {
                    wakes += 1;
                }
                zero_run = b == 0;
                if let Some(Ok(frame)) = dec.push(b) {
                    if let Message::GetFieldStatus(_) = frame.message {
                        requests += 1;
                        if requests == 1 {
                            continue; // swallow the first: force a retry
                        }
                        let fs = FieldStatus {
                            monotonic_us: 1,
                            rtc_unix_ms: 1_793_000_000_000,
                            next_pulse_unix_s: 0,
                            schedule_flags: FIELD_STATUS_RTC_VALID,
                            windows_pending: 0,
                            windows_dropped: 0,
                            oldest_pending_run_id: 0,
                            last_run_id: 0,
                            calr_ppb: 0,
                            last_rtc_step_ms: 0,
                            boot_count: 7,
                            v24_raw: 0,
                            flash_used_kib: 0,
                            schedule_crc: 0x7d54,
                        };
                        let mut out = [0u8; MAX_ENCODED_LEN];
                        let n = encode(&Message::FieldStatus(fs), 0, &mut out).unwrap();
                        board.write_all(&out[..n]).unwrap();
                    }
                }
            }
        }
        // Keep the master open until the host has read the reply.
        std::thread::sleep(Duration::from_millis(500));
        wakes
    });

    let (fs, _, _) = link.field_status().expect("reply after one retry");
    assert_eq!(fs.boot_count, 7);
    assert_eq!(link.retries_used, 1);
    let wakes = worker.join().unwrap();
    assert_eq!(wakes, 2, "one wake byte per attempt");
}
