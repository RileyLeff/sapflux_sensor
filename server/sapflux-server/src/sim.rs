//! Gateway simulator: plays the Feather's role against a server (CheckIn
//! with board frames, time sync, WindowPart upload with resends, Events),
//! with a simulated board behind it.

use crate::board::{self, FieldStatus};
use crate::window::{self, RawFrame, WindowParams};
use anyhow::{bail, Context, Result};
use sap_flux_protocol::{BoardState, Hello, Message, Status};
use sapflux_uplink::{
    self as up, Body, CheckIn, CheckInReply, Event, EventAck, Header, Schedule, WindowAck, WindowPart,
    MAX_DATAGRAM_LEN, MAX_PART_DATA,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;

/// Simulated board state behind the gateway.
#[derive(Debug, Clone)]
pub struct SimBoard {
    pub board_id: u64,
    pub firmware_version: u32,
    pub schedule: Schedule,
    pub field_status: FieldStatus,
    pub frame_seq: u32,
}

impl SimBoard {
    /// A board with factory (disabled) schedule.
    pub fn new(board_id: u64) -> SimBoard {
        let schedule = Schedule::FACTORY_DEFAULT;
        SimBoard {
            board_id,
            firmware_version: 0x00_02_00, // 0.2.0
            schedule,
            field_status: FieldStatus {
                schedule_crc: schedule.crc16(),
                schedule_flags: 1 << 3 | 1 << 5,
                boot_count: 1,
                ..Default::default()
            },
            frame_seq: 0,
        }
    }

    /// Apply a `SetSchedule` from the gateway.
    pub fn set_schedule(&mut self, s: Schedule) {
        self.schedule = s;
        self.field_status.schedule_crc = s.crc16();
        self.field_status.schedule_flags =
            (self.field_status.schedule_flags & !1) | s.enabled;
    }

    /// The Hello + Status + FieldStatus frames a `Query` + `GetFieldStatus`
    /// would return, concatenated as the CheckIn carries them.
    pub fn frames(&mut self, now_ms: i64) -> Vec<u8> {
        self.frame_seq += 3;
        let s = self.frame_seq;
        let mono = (now_ms as u64 % 1_000_000_000) * 1000;
        self.field_status.monotonic_us = mono;
        self.field_status.rtc_unix_ms = now_ms;
        let mut out = board::build_crate_frame(
            &Message::Hello(Hello {
                board_id: self.board_id,
                firmware_version: self.firmware_version,
                capabilities: 15,
            }),
            s - 2,
        );
        out.extend(board::build_crate_frame(
            &Message::Status(Status {
                monotonic_us: mono,
                rtc_unix_ms: now_ms,
                run_id: 0,
                state: BoardState::SafeIdle,
                fault_code: 0,
                pulse_duration_ms: self.schedule.pulse_duration_ms,
                cooldown_remaining_ms: 0,
            }),
            s - 1,
        ));
        out.extend(board::field_status_frame(&self.field_status, s));
        out
    }
}

/// Result of a check-in.
#[derive(Debug, Clone, Copy)]
pub struct CheckInOutcome {
    pub reply: CheckInReply,
    /// Server time minus gateway time.
    pub offset_ms: i64,
    pub rtt_ms: i64,
    /// Whether the gateway pushed `SetSchedule` to the board.
    pub pushed_schedule: bool,
}

/// The simulated gateway.
pub struct Gateway {
    sock: UdpSocket,
    server: SocketAddr,
    pub device_id: u64,
    key: Vec<u8>,
    pub boot_id: u32,
    pub seq: u32,
    /// Added to the real clock to model a wrong gateway clock.
    pub clock_skew_ms: i64,
    pub timeout: Duration,
    pub retries: u32,
    /// Raw datagrams sent, for replay tests.
    pub sent: Vec<Vec<u8>>,
}

impl Gateway {
    /// Bind a local socket.
    pub async fn new(server: SocketAddr, device_id: u64, key: Vec<u8>, boot_id: u32) -> Result<Gateway> {
        let bind: SocketAddr = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse()?;
        Ok(Gateway {
            sock: UdpSocket::bind(bind).await?,
            server,
            device_id,
            key,
            boot_id,
            seq: 0,
            clock_skew_ms: 0,
            timeout: Duration::from_millis(1500),
            retries: 3,
            sent: Vec::new(),
        })
    }

    /// Gateway clock.
    pub fn now_ms(&self) -> i64 {
        crate::now_ms() + self.clock_skew_ms
    }

    /// Send raw bytes and wait for one datagram (no retries).
    pub async fn send_raw(&self, dg: &[u8], wait: Duration) -> Result<Option<Vec<u8>>> {
        self.sock.send_to(dg, self.server).await?;
        let mut buf = vec![0u8; 2048];
        match tokio::time::timeout(wait, self.sock.recv_from(&mut buf)).await {
            Ok(r) => {
                let (n, _) = r?;
                Ok(Some(buf[..n].to_vec()))
            }
            Err(_) => Ok(None),
        }
    }

    /// Send `body` with a fresh seq, retrying (new seq each time) until a
    /// reply with the matching type, boot_id and seq arrives.
    async fn exchange(&mut self, body: &Body<'_>, reply_type: u8) -> Result<(Vec<u8>, i64)> {
        for _ in 0..=self.retries {
            self.seq += 1;
            let header = Header {
                msg_type: body.msg_type(),
                device_id: self.device_id,
                boot_id: self.boot_id,
                seq: self.seq,
            };
            let mut out = [0u8; MAX_DATAGRAM_LEN];
            let n = up::seal(&header, body, &self.key, &mut out)?;
            self.sent.push(out[..n].to_vec());
            self.sock.send_to(&out[..n], self.server).await?;
            let deadline = tokio::time::Instant::now() + self.timeout;
            let mut buf = vec![0u8; 2048];
            loop {
                let r = tokio::time::timeout_at(deadline, self.sock.recv_from(&mut buf)).await;
                let Ok(r) = r else { break };
                let (len, _) = r?;
                let t4 = self.now_ms();
                let Ok((h, _)) = up::open(&buf[..len], &self.key) else { continue };
                if h.msg_type == reply_type && h.boot_id == self.boot_id && h.seq == self.seq {
                    return Ok((buf[..len].to_vec(), t4));
                }
            }
        }
        bail!("no reply after {} attempts", self.retries + 1)
    }

    /// CheckIn with the board's frames; applies the reply to the board the
    /// way the Feather does (SetSchedule if `schedule_crc` differs).
    pub async fn check_in(&mut self, board: &mut SimBoard) -> Result<CheckInOutcome> {
        let t1 = self.now_ms();
        let frames = board.frames(t1);
        let body = Body::CheckIn(CheckIn {
            t1_unix_ms: t1,
            gw_fw: 0x0000_0100,
            batt_mv: 3310,
            rsrp_dbm: -97,
            uptime_s: 12,
            flags: up::CHECKIN_FLAG_TIME_VALID | up::CHECKIN_FLAG_BOARD_LINK_OK,
            board_frames: &frames,
        });
        let (dg, t4) = self.exchange(&body, up::TYPE_CHECKIN_REPLY).await?;
        let Body::CheckInReply(reply) = up::open(&dg, &self.key)?.1 else {
            bail!("not a CheckInReply")
        };
        if reply.t1_unix_ms != t1 {
            bail!("t1 echo mismatch");
        }
        let (offset_ms, rtt_ms) = reply.offset_rtt(t4);
        let pushed_schedule = board.field_status.schedule_crc != reply.schedule.crc16();
        if pushed_schedule {
            up::validate_schedule(&reply.schedule)
                .map_err(|e| anyhow::anyhow!("server sent an invalid schedule: {}", up::schedule_error_text(e)))?;
            board.set_schedule(reply.schedule);
        }
        Ok(CheckInOutcome {
            reply,
            offset_ms,
            rtt_ms,
            pushed_schedule,
        })
    }

    /// Upload a window (WindowInfo frame + data). `drop_first` lists parts
    /// to "lose" on the first pass; missing parts are resent from the ack
    /// bitmap for up to `max_passes` passes.
    pub async fn send_window(
        &mut self,
        board_id: u64,
        run_id: u32,
        bytes: &[u8],
        drop_first: &[u16],
        max_passes: u32,
    ) -> Result<(WindowAck, u32)> {
        let parts: Vec<&[u8]> = bytes.chunks(MAX_PART_DATA).collect();
        let part_count = parts.len() as u16;
        let mut todo: Vec<u16> = (0..part_count).collect();
        let mut sent = 0;
        let mut last: Option<WindowAck> = None;
        for pass in 0..max_passes {
            for &i in &todo {
                if pass == 0 && drop_first.contains(&i) {
                    continue;
                }
                let body = Body::WindowPart(WindowPart {
                    board_id,
                    run_id,
                    part: i,
                    part_count,
                    total_len: bytes.len() as u32,
                    data: parts[i as usize],
                });
                let (dg, _) = self.exchange(&body, up::TYPE_WINDOW_ACK).await?;
                let Body::WindowAck(ack) = up::open(&dg, &self.key)?.1 else {
                    bail!("not a WindowAck")
                };
                sent += 1;
                last = Some(ack);
                if ack.complete {
                    return Ok((ack, sent));
                }
            }
            let ack = last.context("no ack")?;
            todo = ack.missing().collect();
            if todo.is_empty() {
                // All parts stored but not complete: verification failed and
                // the server cleared the bitmap; resend everything.
                todo = (0..part_count).collect();
            }
        }
        bail!("window {run_id} not complete after {max_passes} passes")
    }

    /// Send an Event.
    pub async fn event(&mut self, code: u16, text: &str) -> Result<EventAck> {
        let t = self.now_ms();
        let body = Body::Event(Event {
            t_unix_ms: t,
            code,
            text: text.as_bytes(),
        });
        let (dg, _) = self.exchange(&body, up::TYPE_EVENT_ACK).await?;
        let Body::EventAck(a) = up::open(&dg, &self.key)?.1 else {
            bail!("not an EventAck")
        };
        if a.t_unix_ms != t {
            bail!("EventAck echo mismatch");
        }
        Ok(a)
    }
}

/// A synthetic pulse window: 12 thermistors with a heat-pulse bump
/// (downstream larger than upstream), excitation flat, 25 Hz frames, and a
/// short dropout on probe-2 T6 to exercise the sentinel.
pub fn synthetic_window(run_id: u32, heater_on_unix_ms: i64, schedule: &Schedule) -> (Vec<u8>, Vec<RawFrame>) {
    let heater_on_us: u64 = 3_600_000_000;
    let hz = schedule.sample_hz as u64;
    let period_us = 1_000_000 / hz;
    let start = heater_on_us - schedule.baseline_s as u64 * 1_000_000 - 2_000_000;
    let span = (schedule.baseline_s as u64 + schedule.recovery_s as u64 + 5) * 1_000_000;
    let frames: Vec<RawFrame> = (0..span / period_us)
        .map(|i| {
            let m = start + i * period_us;
            let t = (m as f64 - heater_on_us as f64) / 1e6;
            let mut ch = [None; 13];
            for (c, v) in ch.iter_mut().take(12).enumerate() {
                let amp = if c % 6 < 3 { 260.0 } else { 180.0 } - 20.0 * (c % 3) as f64;
                let tau = 25.0 + 3.0 * c as f64;
                let bump = if t > 0.0 { amp * (t / tau) * (1.0 - t / tau).exp() } else { 0.0 };
                let noise = (((i * 7919 + c as u64 * 104_729) % 13) as f64 - 6.0) * 0.5;
                *v = Some((11_000.0 + 40.0 * c as f64 + 0.01 * t - bump + noise).round() as i16);
            }
            ch[12] = Some(24_880 + ((i % 5) as i16 - 2));
            if (90.0..92.0).contains(&t) {
                ch[11] = None;
            }
            RawFrame {
                monotonic_us: m,
                rtc_unix_ms: heater_on_unix_ms + (t * 1000.0).round() as i64,
                v24_raw: 2990,
                ch,
            }
        })
        .collect();
    let (bytes, _) = window::bin_window(
        &frames,
        &WindowParams {
            run_id,
            window_flags: 0,
            heater_on_us,
            baseline_s: schedule.baseline_s,
            recovery_s: schedule.recovery_s,
            pulse_duration_ms: schedule.pulse_duration_ms,
            sample_hz: schedule.sample_hz,
            bin_ms: schedule.bin_ms,
        },
    );
    (bytes, frames)
}

/// Build a window from a `sap-flux-monitor` capture the way the board will:
/// decimate 50 Hz → 25 Hz, 60 s baseline before the first heater-on frame,
/// 1000 ms pulse, 180 s recovery, 500 ms bins. Probe 2 is absent in the
/// capture, so its channels are all sentinel. Returns `(run_id, bytes)`.
pub fn capture_window(path: &std::path::Path) -> Result<(u32, Vec<u8>)> {
    let (frames, onset) = window::read_capture(path)?;
    let onset = onset.context("capture has no heater-on samples")?;
    let frames = window::decimate(&frames, 2);
    let run_id = frames
        .iter()
        .find(|f| f.monotonic_us >= onset)
        .map(|f| (f.rtc_unix_ms / 1000) as u32)
        .unwrap_or(1);
    let (bytes, _) = window::bin_window(
        &frames,
        &WindowParams {
            run_id,
            window_flags: 0,
            heater_on_us: onset,
            baseline_s: 60,
            recovery_s: 180,
            pulse_duration_ms: 1000,
            sample_hz: 25,
            bin_ms: 500,
        },
    );
    Ok((run_id, bytes))
}
