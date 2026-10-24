//! Link and window-download tests against an in-memory fake board that
//! implements the field-mode command handling from docs/field-protocol.md.

use sap_flux_field_cli::link::{Link, LinkError, LinkOptions, Transport};
use sap_flux_field_cli::window::{self, Window, WindowError};
use sap_flux_protocol::field::{encode_bin_value, mean_raw_rounded};
use sap_flux_protocol::*;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

/// One transmission the host made.
#[derive(Debug, Clone)]
enum TxEvent {
    Bytes(Vec<u8>),
    Sleep(Duration),
}

struct FakeBoard {
    decoder: Decoder,
    rx: VecDeque<u8>,
    seq: u32,
    log: Vec<TxEvent>,
    commands: Vec<Message>,
    /// Replies to this many upcoming commands are swallowed.
    drop_replies: u32,
    /// Corrupt this chunk index's data (after CRC was computed).
    corrupt_chunk: Option<u16>,
    schedule: Schedule,
    schedule_crc: u16,
    rtc_offset_ms: i64,
    windows: BTreeMap<u32, (WindowInfo, Vec<u8>)>,
}

impl FakeBoard {
    fn new() -> Self {
        Self {
            decoder: Decoder::new(),
            rx: VecDeque::new(),
            seq: 0,
            log: Vec::new(),
            commands: Vec::new(),
            drop_replies: 0,
            corrupt_chunk: None,
            schedule: Schedule::FACTORY_DEFAULT,
            schedule_crc: Schedule::FACTORY_DEFAULT.crc16(),
            rtc_offset_ms: 1234,
            windows: BTreeMap::new(),
        }
    }

    fn reply(&mut self, msg: Message) {
        let mut buf = [0u8; MAX_ENCODED_LEN];
        let n = encode(&msg, self.seq, &mut buf).unwrap();
        self.seq += 1;
        self.rx.extend(&buf[..n]);
    }

    fn now_ms(&self) -> i64 {
        sap_flux_field_cli::link::host_unix_ms() + self.rtc_offset_ms
    }

    fn field_status(&self) -> FieldStatus {
        FieldStatus {
            monotonic_us: 1_000_000,
            rtc_unix_ms: self.now_ms(),
            next_pulse_unix_s: if self.schedule.is_enabled() {
                1_793_003_600
            } else {
                0
            },
            schedule_flags: (self.schedule.is_enabled() as u8 * FIELD_STATUS_ENABLED)
                | FIELD_STATUS_RTC_VALID,
            windows_pending: self.windows.len() as u8,
            windows_dropped: 0,
            oldest_pending_run_id: self.windows.keys().next().copied().unwrap_or(0),
            last_run_id: self.windows.keys().last().copied().unwrap_or(0),
            calr_ppb: 0,
            last_rtc_step_ms: 0,
            boot_count: 1,
            v24_raw: 24000,
            flash_used_kib: 0,
            schedule_crc: self.schedule_crc,
        }
    }

    fn ack(&mut self, command_type: u8, result: u8, nonce: u64, run_id: u32) {
        self.reply(Message::Ack(Ack {
            command_type,
            result,
            command_nonce: nonce,
            run_id,
        }));
    }

    fn handle(&mut self, msg: Message) {
        self.commands.push(msg);
        if self.drop_replies > 0 {
            self.drop_replies -= 1;
            return;
        }
        match msg {
            Message::Query(q) => {
                self.ack(MSG_QUERY, 0, q.command_nonce, 0);
                self.reply(Message::Hello(Hello {
                    board_id: 0x1122_3344_5566_7788,
                    firmware_version: 0x0002_0000,
                    capabilities: CAP_RTC | CAP_ADC | CAP_V24_MONITOR | CAP_HEATER_CONTROL,
                }));
                self.reply(Message::Status(Status {
                    monotonic_us: 1,
                    rtc_unix_ms: self.now_ms(),
                    run_id: 0,
                    state: BoardState::SafeIdle,
                    fault_code: 0,
                    pulse_duration_ms: 1000,
                    cooldown_remaining_ms: 0,
                }));
                let fs = self.field_status();
                self.reply(Message::FieldStatus(fs));
            }
            Message::GetFieldStatus(_) => {
                let fs = self.field_status();
                self.reply(Message::FieldStatus(fs));
            }
            Message::SetSchedule(s) => {
                let result = match s.schedule.validate(600_000) {
                    Ok(()) => {
                        self.schedule = s.schedule;
                        self.schedule_crc = s.schedule.crc16();
                        ACK_OK
                    }
                    Err(_) => ACK_INVALID_PARAMETER,
                };
                self.ack(MSG_SET_SCHEDULE, result, s.command_nonce, 0);
                let fs = self.field_status();
                self.reply(Message::FieldStatus(fs));
            }
            Message::SetRtc(r) => {
                self.rtc_offset_ms = r.unix_ms - sap_flux_field_cli::link::host_unix_ms();
                self.ack(MSG_SET_RTC, 0, r.command_nonce, 0);
                self.reply(Message::Status(Status {
                    monotonic_us: 2,
                    rtc_unix_ms: self.now_ms(),
                    run_id: 0,
                    state: BoardState::SafeIdle,
                    fault_code: 0,
                    pulse_duration_ms: 1000,
                    cooldown_remaining_ms: 0,
                }));
            }
            Message::GetWindow(g) => {
                let key = if g.run_id == WINDOW_OLDEST {
                    self.windows.keys().next().copied()
                } else {
                    Some(g.run_id).filter(|k| self.windows.contains_key(k))
                };
                let Some(key) = key else {
                    return self.ack(MSG_GET_WINDOW, ACK_NOT_FOUND, g.command_nonce, g.run_id);
                };
                let (info, data) = self.windows[&key].clone();
                if g.chunk == CHUNK_HEADER {
                    return self.reply(Message::WindowInfo(info));
                }
                match field::chunk_range(g.chunk, data.len()) {
                    Some(r) => {
                        let mut bytes = data[r].to_vec();
                        if self.corrupt_chunk == Some(g.chunk) {
                            bytes[0] ^= 0x40;
                        }
                        let c = WindowChunk::new(info.run_id, g.chunk, &bytes).unwrap();
                        self.reply(Message::WindowChunk(c));
                    }
                    None => self.ack(MSG_GET_WINDOW, ACK_NOT_FOUND, g.command_nonce, g.run_id),
                }
            }
            Message::AckWindow(a) => {
                let result = if self.windows.remove(&a.run_id).is_some() {
                    ACK_OK
                } else {
                    ACK_NOT_FOUND
                };
                self.ack(MSG_ACK_WINDOW, result, a.command_nonce, a.run_id);
            }
            _ => {}
        }
    }

    /// Add a synthetic default-schedule window (482 bins × 13 channels).
    fn add_window(&mut self, run_id: u32) -> (WindowInfo, Vec<u8>) {
        let n_bins = Schedule::FACTORY_DEFAULT.window_bins() as usize;
        let count = 12u32; // 25 Hz × 500 ms ≈ 12 frames per bin
                           // Raw readings: per-channel offset, a heat bump after bin 122, and
                           // a quarter-count ripple so 1/16 resolution matters.
        let raw = |k: usize, c: usize, j: u32| -> i32 {
            let base = 12000 + 100 * c as i32;
            let heat = if k >= 122 {
                ((k - 122) as i32 * 7).min(900)
            } else {
                0
            };
            base + heat + (j as i32 % 4) - 1
        };
        let mut base_raw = [0i16; 13];
        for (c, b) in base_raw.iter_mut().enumerate() {
            let sum: i32 = (0..count).map(|j| raw(0, c, j)).sum();
            *b = mean_raw_rounded(sum, count).unwrap();
        }
        let mut data = Vec::with_capacity(n_bins * 26);
        for k in 0..n_bins {
            for (c, &base) in base_raw.iter().enumerate() {
                let v = if k == 200 && c == 4 {
                    encode_bin_value(0, 0, base) // missing
                } else {
                    let sum: i32 = (0..count).map(|j| raw(k, c, j)).sum();
                    encode_bin_value(sum, count, base)
                };
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
        let info = WindowInfo {
            run_id,
            window_flags: 0,
            channels: 13,
            bin_ms_div10: 50,
            n_bins: n_bins as u16,
            chunk_count: field::chunk_count(data.len()) as u16,
            start_monotonic_us: 10_000_000,
            start_rtc_unix_ms: 1_793_000_000_000,
            end_monotonic_us: 10_000_000 + n_bins as u64 * 500_000,
            end_rtc_unix_ms: 1_793_000_000_000 + n_bins as i64 * 500,
            heater_on_offset_ms: 60_000,
            pulse_duration_ms: 1000,
            sample_hz: 25,
            v24_mean_raw: 24000,
            frames_total: 6025,
            frames_missing: 0,
            data_crc32: crc::crc32(&data),
            base_raw,
        };
        self.windows.insert(run_id, (info, data.clone()));
        (info, data)
    }
}

impl Transport for FakeBoard {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.log.push(TxEvent::Bytes(bytes.to_vec()));
        for &b in bytes {
            if let Some(Ok(frame)) = self.decoder.push(b) {
                self.handle(frame.message);
            }
        }
        Ok(())
    }
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.rx.is_empty() {
            std::thread::sleep(Duration::from_millis(1));
            return Ok(0);
        }
        let n = buf.len().min(self.rx.len()).min(37); // arrive in pieces
        for b in buf.iter_mut().take(n) {
            *b = self.rx.pop_front().unwrap();
        }
        Ok(n)
    }
    fn clear_input(&mut self) -> io::Result<()> {
        self.rx.clear();
        Ok(())
    }
    fn sleep(&mut self, d: Duration) {
        self.log.push(TxEvent::Sleep(d));
    }
}

fn link() -> Link<FakeBoard> {
    Link::new(
        FakeBoard::new(),
        LinkOptions {
            timeout: Duration::from_millis(40),
            linger: Duration::from_millis(10),
            ..LinkOptions::default()
        },
    )
}

#[test]
fn every_command_is_preceded_by_wake_byte_and_pause() {
    let mut l = link();
    l.query().unwrap();
    l.field_status().unwrap();
    l.transport_mut().drop_replies = 1;
    l.field_status().unwrap();
    let log = &l.transport_mut().log;
    let mut frames = 0;
    for (i, ev) in log.iter().enumerate() {
        if let TxEvent::Bytes(b) = ev {
            if b.as_slice() == [0x00] {
                continue;
            }
            frames += 1;
            assert!(i >= 2, "frame {i} lacks a wake prefix");
            match (&log[i - 2], &log[i - 1]) {
                (TxEvent::Bytes(w), TxEvent::Sleep(d)) => {
                    assert_eq!(w.as_slice(), [0x00], "single wake byte");
                    assert!(*d >= Duration::from_millis(10), "pause {d:?}");
                }
                other => panic!("bad prefix {other:?}"),
            }
        }
    }
    assert_eq!(frames, 4, "3 commands + 1 retry");
}

#[test]
fn query_collects_hello_status_and_field_status() {
    let mut l = link();
    let q = l.query().unwrap();
    assert_eq!(q.hello.board_id, 0x1122_3344_5566_7788);
    assert_eq!(q.status.state, BoardState::SafeIdle);
    assert!(q.field_status.is_some());
    assert!(
        q.status.rtc_unix_ms - q.status_rx_unix_ms > 1000,
        "fake offset visible"
    );
}

#[test]
fn retries_up_to_three_times_then_times_out() {
    let mut l = link();
    l.transport_mut().drop_replies = 3;
    l.field_status().unwrap();
    assert_eq!(l.retries_used, 3);
    assert_eq!(l.frames_sent, 4);

    let mut l = link();
    l.transport_mut().drop_replies = 4;
    match l.field_status() {
        Err(LinkError::Timeout { attempts, .. }) => assert_eq!(attempts, 4),
        other => panic!("{other:?}"),
    }
    assert_eq!(l.frames_sent, 4);
}

#[test]
fn retries_reuse_the_nonce() {
    let mut l = link();
    l.transport_mut().drop_replies = 2;
    l.ack_window(5).unwrap();
    let nonces: Vec<u64> = l
        .transport_mut()
        .commands
        .iter()
        .map(|m| match m {
            Message::AckWindow(a) => a.command_nonce,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(nonces.len(), 3);
    assert!(nonces.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn set_schedule_acks_and_reports_crc() {
    let mut l = link();
    let schedule = Schedule {
        enabled: 1,
        ..Schedule::FACTORY_DEFAULT
    };
    let r = l
        .set_schedule(SetSchedule {
            command_nonce: 0,
            schedule,
            lease_until_unix_s: 0,
        })
        .unwrap();
    assert_eq!(r.ack.result, ACK_OK);
    let fs = r.field_status.expect("FieldStatus follows the Ack");
    assert_eq!(fs.schedule_crc, schedule.crc16());
    assert_ne!(fs.schedule_flags & FIELD_STATUS_ENABLED, 0);
}

#[test]
fn rejected_schedule_is_not_retried() {
    let mut l = link();
    let r = l
        .set_schedule(SetSchedule {
            command_nonce: 0,
            schedule: Schedule {
                interval_s: 100,
                ..Schedule::FACTORY_DEFAULT
            },
            lease_until_unix_s: 0,
        })
        .unwrap();
    assert_eq!(r.ack.result, ACK_INVALID_PARAMETER);
    assert_eq!(l.frames_sent, 1);
}

#[test]
fn set_rtc_is_restamped_on_retry() {
    let mut l = link();
    l.transport_mut().drop_replies = 1;
    let (r, sent) = l.set_rtc_now(3).unwrap();
    assert_eq!(r.ack.result, 0);
    assert!(r.status.is_some());
    let stamps: Vec<i64> = l
        .transport_mut()
        .commands
        .iter()
        .map(|m| match m {
            Message::SetRtc(s) => s.unix_ms,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(stamps.len(), 2);
    assert!(
        stamps[1] > stamps[0],
        "second attempt re-stamped: {stamps:?}"
    );
    assert_eq!(stamps[1], sent);
    let q = l.query().unwrap();
    let off = q.status.rtc_unix_ms - (q.sent_unix_ms + q.status_rx_unix_ms) / 2;
    assert!(off.abs() < 50, "offset after SetRtc {off}");
}

#[test]
fn download_verify_csv_and_ack() {
    let mut l = link();
    let (info, data) = l.transport_mut().add_window(41);
    l.transport_mut().add_window(42);
    assert_eq!(info.chunk_count, 126);
    assert_eq!(data.len(), 12_532);

    // Run 0 = oldest = 41; a dropped chunk reply is retried transparently.
    l.transport_mut().drop_replies = 0;
    let mut progress = Vec::new();
    let w = window::download(&mut l, WINDOW_OLDEST, |d, t| progress.push((d, t))).unwrap();
    assert_eq!(w.info, info);
    assert_eq!(w.data, data);
    assert_eq!(progress.last(), Some(&(126, 126)));
    assert_eq!(w.uplink_bytes().len(), WINDOW_INFO_FRAME_LEN + 12_532);
    assert_eq!(
        decode_frame(&w.uplink_bytes()[..WINDOW_INFO_FRAME_LEN])
            .unwrap()
            .message,
        Message::WindowInfo(info)
    );

    let mut csv = Vec::new();
    w.write_csv(&mut csv).unwrap();
    let csv = String::from_utf8(csv).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 1 + 482);
    let header: Vec<&str> = lines[0].split(',').collect();
    assert_eq!(header.len(), 5 + 13);
    assert_eq!(header[5], "t1_raw");
    assert_eq!(header[11], "p2t1_raw");
    assert_eq!(header[17], "excitation_raw");
    // Bin 0: the base, ripple mean (−1,0,1,2 ×3)/12 = 0.5 → 12000.5.
    let row0: Vec<&str> = lines[1].split(',').collect();
    assert_eq!(row0[1], "0");
    assert_eq!(row0[2], "-60.000");
    assert_eq!(row0[4], "1793000000000");
    assert_eq!(row0[5], "12000.5000");
    // Bin 300, channel 12 (excitation): base 13200, heat min(178*7, 900)=900.
    let row300: Vec<&str> = lines[301].split(',').collect();
    assert_eq!(row300[4], "1793000150000");
    assert_eq!(row300[17], "14100.5000");
    // The missing value is an empty field.
    let row200: Vec<&str> = lines[201].split(',').collect();
    assert_eq!(row200[5 + 4], "");

    let r = l.ack_window(41).unwrap();
    assert_eq!(r.ack.result, ACK_OK);
    // Oldest is now 42; 41 is gone and NOT_FOUND is terminal (no retry).
    let sent_before = l.frames_sent;
    assert!(matches!(l.window_info(41), Err(LinkError::NotFound)));
    assert_eq!(l.frames_sent, sent_before + 1);
    assert_eq!(l.window_info(0).unwrap().run_id, 42);
    assert!(matches!(l.window_chunk(42, 126), Err(LinkError::NotFound)));
}

#[test]
fn chunk_retry_mid_download() {
    let mut l = link();
    l.transport_mut().add_window(7);
    // Drop the header reply and the next one (chunk 0).
    l.transport_mut().drop_replies = 2;
    let w = window::download(&mut l, 7, |_, _| {}).unwrap();
    assert!(w.verify().is_ok());
    assert_eq!(l.retries_used, 2);
}

#[test]
fn corrupted_data_fails_crc() {
    let mut l = link();
    l.transport_mut().add_window(9);
    l.transport_mut().corrupt_chunk = Some(17);
    match window::download(&mut l, 9, |_, _| {}) {
        Err(WindowError::Verify(msg)) => assert!(msg.contains("data_crc32"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn assemble_rejects_bad_chunks() {
    let mut board = FakeBoard::new();
    let (info, data) = board.add_window(3);
    let chunks: Vec<WindowChunk> = (0..info.chunk_count)
        .map(|c| {
            let r = field::chunk_range(c, data.len()).unwrap();
            WindowChunk::new(3, c, &data[r]).unwrap()
        })
        .collect();
    assert!(Window::assemble(info, &chunks).is_ok());
    // Out of order is fine.
    let mut rev = chunks.clone();
    rev.reverse();
    assert!(Window::assemble(info, &rev).is_ok());
    // Missing chunk.
    assert!(Window::assemble(info, &chunks[1..]).is_err());
    // Wrong run id.
    let mut bad = chunks.clone();
    bad[5].run_id = 4;
    assert!(Window::assemble(info, &bad).is_err());
    // Wrong length on the last chunk.
    let mut bad = chunks.clone();
    let last = bad.len() - 1;
    bad[last] = WindowChunk::new(3, last as u16, &[0u8; 100]).unwrap();
    assert!(Window::assemble(info, &bad).is_err());
}

#[test]
fn rtc_interpolation_and_time_invalid() {
    let mut board = FakeBoard::new();
    let (mut info, data) = board.add_window(3);
    let w = Window {
        info,
        data: data.clone(),
    };
    assert_eq!(w.rtc_at(info.start_monotonic_us), Some(1_793_000_000_000.0));
    assert_eq!(
        w.rtc_at(info.end_monotonic_us),
        Some(info.end_rtc_unix_ms as f64)
    );
    info.window_flags |= WINDOW_FLAG_TIME_INVALID;
    let w = Window { info, data };
    assert_eq!(w.rtc_at(info.start_monotonic_us), None);
    let mut csv = Vec::new();
    w.write_csv(&mut csv).unwrap();
    let csv = String::from_utf8(csv).unwrap();
    let row0: Vec<&str> = csv.lines().nth(1).unwrap().split(',').collect();
    assert_eq!(row0[4], "", "no RTC column when TIME_INVALID");
}
