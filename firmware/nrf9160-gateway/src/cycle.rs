//! One gateway cycle (docs/field-protocol.md, "Gateway cycle"):
//!
//! 1. (main) Wake; P0.31 is already high.
//! 2. Wake the board, `Query` + `GetFieldStatus`.
//! 3. Attach LTE-M if needed, `CheckIn`, apply the reply's time, push
//!    `SetSchedule` if `schedule_crc` differs or a lease is due, `SetRtc` if
//!    more than 250 ms off.
//! 4. For each pending window: header + chunks, verify `data_crc32`,
//!    `WindowPart`s, `AckWindow` on `complete`.
//! 5. Return how long to sleep: the sooner of `wake_interval_s` and 60 s
//!    after the board's next window ends.
//!
//! If the server is unreachable nothing is synced, the board's schedule is
//! left alone, and windows stay on the board.

use arrayvec::ArrayVec;
use core::fmt::Write;
use embassy_time::{Duration, Instant};
use sap_flux_protocol::{
    self as proto, AckWindow, BoardState, FieldStatus, GetFieldStatus, GetWindow, Message, Query,
    Schedule, SetRtc, SetSchedule, Status, WindowInfo,
};
use sapflux_uplink::{
    self as up, Body, CHECKIN_FLAG_BOARD_LINK_OK, CHECKIN_FLAG_TIME_VALID, CheckIn, CheckInReply,
    REQUEST_REBOOT, REQUEST_SEND_WINDOWS,
};

use crate::board::{BoardLink, Expect, LinkError, RawFrame, Reply as BoardReply};
use crate::clock::Clock;
use crate::events::{self, Events};
use crate::modem::{self, Modem};
use crate::uplink::{Reply, Uplink};
use crate::{event, info, wdog};

/// Default check-in period until the server sends one.
pub const DEFAULT_WAKE_INTERVAL_S: u32 = 3600;
const MIN_WAKE_INTERVAL_S: u32 = 60;
const MAX_WAKE_INTERVAL_S: u32 = 86_400;
/// Never sleep less than this (a window may be running right now).
const MIN_SLEEP_S: u32 = 30;
/// Post-window wake margin (spec: 60 s after the window ends).
const POST_WINDOW_MARGIN_S: u32 = 60;
/// SetRtc threshold (spec).
const RTC_STEP_THRESHOLD_MS: i64 = 250;
/// Attach timeout per cycle.
const ATTACH_TIMEOUT_S: u64 = 180;
/// CheckIn: attempts × reply timeout.
const CHECKIN_ATTEMPTS: u32 = 3;
const CHECKIN_TIMEOUT: Duration = Duration::from_secs(6);
/// Window upload resend rounds.
const UPLOAD_ROUNDS: u32 = 5;
/// Space for the WindowInfo frame plus the largest window
/// (300 s baseline + 1.5 s + 600 s recovery at 250 ms bins = 93,756 B).
pub const WINDOW_BUF_LEN: usize = 96 * 1024;

const Q_HELLO_STATUS: &[u8] = &[proto::MSG_HELLO, proto::MSG_STATUS];
const Q_FIELD_STATUS: &[u8] = &[proto::MSG_FIELD_STATUS];
const Q_ACK: &[u8] = &[proto::MSG_ACK];
const Q_ACK_FIELD_STATUS: &[u8] = &[proto::MSG_ACK, proto::MSG_FIELD_STATUS];
const Q_WINDOW_INFO: &[u8] = &[proto::MSG_WINDOW_INFO];
const Q_WINDOW_CHUNK: &[u8] = &[proto::MSG_WINDOW_CHUNK];
/// Consecutive verify failures before a window is abandoned on the board.
const BAD_WINDOW_LIMIT: u8 = 3;
/// SetRtc: the board waits up to 1 s for the next second boundary before
/// it acknowledges, so its Ack can take ~1.1 s (spec: 200 ms).
const SET_RTC_TIMEOUTS_MS: [u64; 4] = [2000, 2000, 2000, 2000];

pub struct Gateway {
    pub modem: Modem,
    pub uplink: Uplink,
    pub clock: Clock,
    pub events: Events,
    pub wake_interval_s: u32,
    pub board_frames: BoardFrames,
    board_link_ok: Option<bool>,
    windows_dropped: Option<u16>,
    /// Consecutive cycles without a server reply.
    pub fail_streak: u32,
    /// Last schedule the server sent (what the board should be running).
    server_schedule: Option<Schedule>,
    pub batt_mv: u16,
    window_buf: &'static mut [u8; WINDOW_BUF_LEN],
    /// Window whose header or data_crc32 failed to verify, and in how many
    /// consecutive fetches; abandoned at BAD_WINDOW_LIMIT so it can't block
    /// the windows queued behind it.
    bad_window: Option<(u32, u8)>,
}

/// Latest raw board frames, forwarded in CheckIn.
#[derive(Default)]
pub struct BoardFrames {
    pub hello: Option<RawFrame>,
    pub status: Option<RawFrame>,
    pub field: Option<RawFrame>,
}

impl BoardFrames {
    fn board_id(&self) -> u64 {
        match self.hello.as_ref().map(|f| f.frame.message) {
            Some(Message::Hello(h)) => h.board_id,
            _ => 0,
        }
    }

    fn field_status(&self) -> Option<(FieldStatus, Instant)> {
        match self.field.as_ref().map(|f| (f.frame.message, f.at)) {
            Some((Message::FieldStatus(s), at)) => Some((s, at)),
            _ => None,
        }
    }

    fn status(&self) -> Option<(Status, Instant)> {
        match self.status.as_ref().map(|f| (f.frame.message, f.at)) {
            Some((Message::Status(s), at)) => Some((s, at)),
            _ => None,
        }
    }

    /// Concatenated raw frames (Hello, Status, FieldStatus) into `out`.
    fn concat<'a>(&self, out: &'a mut [u8; 3 * proto::MAX_FRAME_LEN]) -> &'a [u8] {
        let mut n = 0;
        for f in [&self.hello, &self.status, &self.field].into_iter().flatten() {
            let b = f.bytes();
            out[n..n + b.len()].copy_from_slice(b);
            n += b.len();
        }
        &out[..n]
    }

    fn absorb(&mut self, reply: &BoardReply) {
        for f in reply.frames.iter() {
            match f.msg_type() {
                proto::MSG_HELLO => self.hello = Some(f.clone()),
                proto::MSG_STATUS => self.status = Some(f.clone()),
                proto::MSG_FIELD_STATUS => self.field = Some(f.clone()),
                _ => {}
            }
        }
    }
}

impl Gateway {
    pub fn new(
        modem: Modem,
        uplink: Uplink,
        clock: Clock,
        events: Events,
        window_buf: &'static mut [u8; WINDOW_BUF_LEN],
    ) -> Self {
        Self {
            modem,
            uplink,
            clock,
            events,
            wake_interval_s: DEFAULT_WAKE_INTERVAL_S,
            board_frames: BoardFrames::default(),
            board_link_ok: None,
            windows_dropped: None,
            fail_streak: 0,
            server_schedule: None,
            batt_mv: 0,
            window_buf,
            bad_window: None,
        }
    }

    /// Run one cycle; returns the sleep time in seconds.
    pub async fn run(&mut self, board: &mut BoardLink<'_>) -> u32 {
        self.clock.start_cycle();
        wdog::alive();

        // --- 2. Board status.
        self.poll_board(board).await;

        // --- 3. Attach + CheckIn.
        wdog::alive_for(ATTACH_TIMEOUT_S as u32 + 120);
        if let Err(e) = self
            .modem
            .ensure_attached(Duration::from_secs(ATTACH_TIMEOUT_S))
            .await
        {
            event!(self.events, events::EVENT_MODEM_ERROR, "attach failed: {:?}", e);
            self.fail_streak = self.fail_streak.saturating_add(1);
            self.network_time_fallback().await;
            return self.finish(false);
        }
        wdog::alive();
        let rsrp = match modem::at::<96>("AT+CESQ").await {
            Ok(r) => modem::parse_rsrp(&r),
            Err(_) => 0,
        };

        let reply = match self.check_in(rsrp).await {
            Some(r) => r,
            None => {
                self.network_time_fallback().await;
                return self.finish(false);
            }
        };

        self.apply_reply(&reply);
        let board_ok = self.board_link_ok == Some(true);
        if !board_ok {
            info!("board not answering: schedule, RTC and windows skipped");
        } else if self.clock.synced_this_cycle() {
            self.push_schedule(board, &reply).await;
            self.sync_board_rtc(board).await;
        } else {
            // A reply without a usable time sample (RTT ≥ 3 s): the board's
            // clock and lease are left alone this cycle.
            self.network_time_fallback().await;
        }

        // --- 4. Windows.
        if board_ok && reply.request_flags & REQUEST_SEND_WINDOWS != 0 {
            self.transfer_windows(board).await;
        }

        // Refresh FieldStatus for the sleep calculation.
        if board_ok {
            self.get_field_status(board).await;
        }

        self.send_events().await;

        if reply.request_flags & REQUEST_REBOOT != 0 {
            info!("server requested reboot");
            wdog::request_reset();
            loop {
                embassy_time::Timer::after_secs(5).await;
            }
        }
        self.finish(true)
    }

    // -----------------------------------------------------------------------
    // Board.

    async fn poll_board(&mut self, board: &mut BoardLink<'_>) {
        let nonce = board.next_nonce();
        let q = board
            .transact(
                Expect::new(Q_HELLO_STATUS),
                Message::Query(Query {
                    command_nonce: nonce,
                }),
            )
            .await;
        let ok = match &q {
            Ok(r) => {
                self.board_frames.absorb(r);
                true
            }
            Err(e) => {
                crate::board::log_err("Query", *e);
                false
            }
        };
        if ok {
            self.get_field_status(board).await;
        }
        match (self.board_link_ok, ok) {
            (Some(true) | None, false) => {
                event!(self.events, events::EVENT_BOARD_LINK_LOST, "no reply to Query: {:?}", q.err());
            }
            (Some(false), true) => {
                event!(self.events, events::EVENT_BOARD_LINK_RESTORED, "board answering again");
            }
            _ => {}
        }
        self.board_link_ok = Some(ok);

        if let Some((fs, _)) = self.board_frames.field_status() {
            info!(
                "board {:016x}: pending {} dropped {} next_pulse {} flags 0x{:02x} crc 0x{:04x}",
                self.board_frames.board_id(),
                fs.windows_pending,
                fs.windows_dropped,
                fs.next_pulse_unix_s,
                fs.schedule_flags,
                fs.schedule_crc
            );
            if let Some(prev) = self.windows_dropped {
                if fs.windows_dropped > prev {
                    event!(
                        self.events,
                        events::EVENT_WINDOW_DROPPED,
                        "board dropped {} window(s) (total {})",
                        fs.windows_dropped - prev,
                        fs.windows_dropped
                    );
                }
            } else if fs.windows_dropped > 0 {
                event!(
                    self.events,
                    events::EVENT_WINDOW_DROPPED,
                    "board reports {} dropped window(s) since its boot",
                    fs.windows_dropped
                );
            }
            self.windows_dropped = Some(fs.windows_dropped);
        }
    }

    async fn get_field_status(&mut self, board: &mut BoardLink<'_>) -> bool {
        let nonce = board.next_nonce();
        match board
            .transact(
                Expect::new(Q_FIELD_STATUS),
                Message::GetFieldStatus(GetFieldStatus {
                    command_nonce: nonce,
                }),
            )
            .await
        {
            Ok(r) => {
                self.board_frames.absorb(&r);
                true
            }
            Err(e) => {
                crate::board::log_err("GetFieldStatus", e);
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // Server.

    async fn check_in(&mut self, rsrp: i16) -> Option<CheckInReply> {
        let mut frames_buf = [0u8; 3 * proto::MAX_FRAME_LEN];
        let frames = self.board_frames.concat(&mut frames_buf);
        let mut flags = 0;
        if self.clock.valid() {
            flags |= CHECKIN_FLAG_TIME_VALID;
        }
        if self.board_link_ok == Some(true) {
            flags |= CHECKIN_FLAG_BOARD_LINK_OK;
        }
        let batt_mv = self.batt_mv;
        let clock = &self.clock;
        let uptime_s = Instant::now().as_secs() as u32;
        let mut best: Option<CheckInReply> = None;
        // Each attempt carries a fresh t1; the reply echoes the t1 it
        // answers, so every reply yields a valid sample.
        let r = self
            .uplink
            .exchange(
                CHECKIN_ATTEMPTS,
                CHECKIN_TIMEOUT,
                || {
                    Body::CheckIn(CheckIn {
                        t1_unix_ms: clock.now(),
                        gw_fw: crate::config::FW_VERSION,
                        batt_mv,
                        rsrp_dbm: rsrp,
                        uptime_s,
                        flags,
                        board_frames: frames,
                    })
                },
                |r| matches!(r, Reply::CheckIn(..)),
            )
            .await;
        match r {
            Ok(Reply::CheckIn(reply, t4)) => {
                let (offset, rtt) = reply.offset_rtt(self.clock.at(t4));
                let applied = self.clock.server_sample(offset, rtt);
                info!(
                    "checkin reply: offset {} ms rtt {} ms ({}), cfg v{} wake {} s flags 0x{:x}",
                    offset,
                    rtt,
                    if applied { "applied" } else { "rejected" },
                    reply.config_version,
                    reply.wake_interval_s,
                    reply.request_flags
                );
                best = Some(reply);
                self.fail_streak = 0;
            }
            Ok(_) => {}
            Err(e) => {
                info!("checkin failed: {:?}", e);
                self.fail_streak = self.fail_streak.saturating_add(1);
                self.uplink.reset_socket();
                event!(self.events, events::EVENT_MODEM_ERROR, "no CheckIn reply: {:?}", e);
            }
        }
        best
    }

    fn apply_reply(&mut self, reply: &CheckInReply) {
        if reply.wake_interval_s != 0 {
            self.wake_interval_s = reply
                .wake_interval_s
                .clamp(MIN_WAKE_INTERVAL_S, MAX_WAKE_INTERVAL_S);
        }
    }

    async fn network_time_fallback(&mut self) {
        if self.clock.valid() {
            return;
        }
        if let Ok(r) = modem::at::<64>("AT+CCLK?").await {
            let at = Instant::now();
            if let Some(ms) = modem::parse_cclk(&r) {
                if self.clock.network_sample(ms, at) {
                    info!("clock: network time {} ms ({})", ms, r.trim());
                }
            }
        }
    }

    async fn push_schedule(&mut self, board: &mut BoardLink<'_>, reply: &CheckInReply) {
        let schedule = reply.schedule;
        if schedule.to_bytes() == [0u8; proto::SCHEDULE_LEN] {
            return; // server has no schedule for this board
        }
        if let Err(e) = up::validate_schedule(&schedule) {
            info!("schedule from server invalid: {}", up::schedule_error_text(e));
            return;
        }
        self.server_schedule = Some(schedule);
        let Some((fs, _)) = self.board_frames.field_status() else {
            info!("schedule: no FieldStatus from board, not pushing");
            return;
        };
        let crc_differs = fs.schedule_crc != schedule.crc16();
        let lease_due = schedule.requires_lease() && reply.lease_s > 0;
        if !crc_differs && !lease_due {
            return;
        }
        let lease_until = if schedule.requires_lease() {
            (self.clock.now() / 1000) as u32 + reply.lease_s
        } else {
            0
        };
        let nonce = board.next_nonce();
        let r = board
            .transact(
                Expect::new(Q_ACK_FIELD_STATUS),
                Message::SetSchedule(SetSchedule {
                    command_nonce: nonce,
                    schedule,
                    lease_until_unix_s: lease_until,
                }),
            )
            .await;
        match r {
            Ok(r) => {
                self.board_frames.absorb(&r);
                info!(
                    "schedule pushed (crc 0x{:04x}, lease until {})",
                    schedule.crc16(),
                    lease_until
                );
            }
            Err(LinkError::Rejected(code)) => {
                event!(
                    self.events,
                    events::EVENT_GATEWAY_BOARD_ERROR,
                    "board rejected SetSchedule: result {}",
                    code
                );
            }
            Err(e) => crate::board::log_err("SetSchedule", e),
        }
    }

    async fn sync_board_rtc(&mut self, board: &mut BoardLink<'_>) {
        // Fresh Status so the comparison uses a current board timestamp.
        let nonce = board.next_nonce();
        match board
            .transact(
                Expect::new(Q_HELLO_STATUS),
                Message::Query(Query {
                    command_nonce: nonce,
                }),
            )
            .await
        {
            Ok(r) => self.board_frames.absorb(&r),
            Err(e) => {
                crate::board::log_err("Query (rtc)", e);
                return;
            }
        }
        let Some((status, at)) = self.board_frames.status() else {
            return;
        };
        let rtc_valid = self
            .board_frames
            .field_status()
            .map(|(fs, _)| fs.schedule_flags & proto::FIELD_STATUS_RTC_VALID != 0)
            .unwrap_or(status.rtc_unix_ms > 0);
        let gw_ms = self.clock.at(at);
        let diff = status.rtc_unix_ms - gw_ms;
        if rtc_valid && diff.abs() <= RTC_STEP_THRESHOLD_MS {
            info!("board rtc ok ({} ms)", diff);
            return;
        }
        let precise = self
            .clock
            .best_rtt()
            .is_some_and(|rtt| rtt <= crate::clock::BOARD_STEP_MAX_RTT_MS);
        if rtc_valid && !precise {
            info!("board rtc off by {} ms but sync rtt too high; not stepping", diff);
            return;
        }
        if !matches!(status.state, BoardState::SafeIdle | BoardState::Cooldown) {
            info!("board rtc off by {} ms but state {:?}; not stepping", diff, status.state);
            return;
        }
        let nonce = board.next_nonce();
        let clock = &self.clock;
        let expect = Expect {
            timeouts_ms: &SET_RTC_TIMEOUTS_MS,
            ..Expect::new(Q_ACK)
        };
        // The board treats unix_ms as "now" at receipt; add the 30-byte
        // frame's 2.6 ms at 115200 baud.
        let r = board
            .transact_with(expect, || {
                Message::SetRtc(SetRtc {
                    command_nonce: nonce,
                    unix_ms: clock.now() + 3,
                })
            })
            .await;
        match r {
            Ok(_) => {
                let mut text = arrayvec::ArrayString::<96>::new();
                let _ = write!(
                    text,
                    "old {} new ~{} step {} ms",
                    status.rtc_unix_ms,
                    gw_ms,
                    -diff
                );
                event!(self.events, events::EVENT_RTC_STEP, "{}", text.as_str());
            }
            Err(LinkError::Rejected(code)) => {
                event!(
                    self.events,
                    events::EVENT_GATEWAY_BOARD_ERROR,
                    "board rejected SetRtc: result {}",
                    code
                );
            }
            Err(e) => crate::board::log_err("SetRtc", e),
        }
    }

    async fn transfer_windows(&mut self, board: &mut BoardLink<'_>) {
        let Some((fs, _)) = self.board_frames.field_status() else {
            return;
        };
        let board_id = self.board_frames.board_id();
        let mut remaining = fs.windows_pending;
        while remaining > 0 {
            remaining -= 1;
            wdog::alive();
            let Some((info, len)) = self.fetch_window(board).await else {
                if self.abandon_bad_window(board).await {
                    continue;
                }
                return;
            };
            self.bad_window = None;
            let run_id = info.run_id;
            info!("window {}: {} B fetched, uploading", run_id, len);
            let data = &self.window_buf[..len];
            if let Err(e) = self
                .uplink
                .upload_window(board_id, run_id, data, UPLOAD_ROUNDS)
                .await
            {
                info!("window {}: upload incomplete: {:?}", run_id, e);
                self.uplink.reset_socket();
                return;
            }
            let nonce = board.next_nonce();
            match board
                .transact(
                    Expect::new(Q_ACK),
                    Message::AckWindow(AckWindow {
                        command_nonce: nonce,
                        run_id,
                    }),
                )
                .await
            {
                Ok(_) => info!("window {}: delivered and freed", run_id),
                // Already freed (e.g. a re-ack after a board reset): the
                // server has it, so carry on with the next window.
                Err(LinkError::Rejected(proto::ACK_NOT_FOUND)) => {
                    info!("window {}: already freed on the board", run_id)
                }
                Err(e) => {
                    // The server has it; the board resends it next time
                    // and the server deduplicates.
                    crate::board::log_err("AckWindow", e);
                    return;
                }
            }
        }
    }

    fn note_bad_window(&mut self, run_id: u32) {
        self.bad_window = match self.bad_window {
            Some((id, n)) if id == run_id => Some((id, n.saturating_add(1))),
            _ => Some((run_id, 1)),
        };
    }

    /// Free a window that failed verification BAD_WINDOW_LIMIT times in a
    /// row so the next one can be fetched. Returns true if one was freed.
    async fn abandon_bad_window(&mut self, board: &mut BoardLink<'_>) -> bool {
        let Some((run_id, n)) = self.bad_window else {
            return false;
        };
        if n < BAD_WINDOW_LIMIT {
            return false;
        }
        let nonce = board.next_nonce();
        let r = board
            .transact(
                Expect::new(Q_ACK),
                Message::AckWindow(AckWindow {
                    command_nonce: nonce,
                    run_id,
                }),
            )
            .await;
        match r {
            Ok(_) | Err(LinkError::Rejected(proto::ACK_NOT_FOUND)) => {
                event!(
                    self.events,
                    events::EVENT_WINDOW_DROPPED,
                    "window {} abandoned after {} failed verifications",
                    run_id,
                    n
                );
                self.bad_window = None;
                true
            }
            Err(e) => {
                crate::board::log_err("AckWindow (abandon)", e);
                false
            }
        }
    }

    /// Fetch the oldest pending window into `window_buf`: the raw
    /// WindowInfo frame (102 B) followed by the data. Returns the header
    /// and total length.
    async fn fetch_window(&mut self, board: &mut BoardLink<'_>) -> Option<(WindowInfo, usize)> {
        let nonce = board.next_nonce();
        let r = board
            .transact(
                Expect::new(Q_WINDOW_INFO),
                Message::GetWindow(GetWindow {
                    command_nonce: nonce,
                    run_id: proto::WINDOW_OLDEST,
                    chunk: proto::CHUNK_HEADER,
                }),
            )
            .await;
        let frame = match r {
            Ok(r) => r.find(proto::MSG_WINDOW_INFO)?.clone(),
            Err(LinkError::Rejected(proto::ACK_NOT_FOUND)) => return None,
            Err(e) => {
                crate::board::log_err("GetWindow header", e);
                return None;
            }
        };
        let Message::WindowInfo(info) = frame.frame.message else {
            return None;
        };
        let header = frame.bytes();
        let data_len = info.data_len();
        let total = header.len() + data_len;
        if !info.is_consistent() || header.len() != proto::WINDOW_INFO_FRAME_LEN || total > WINDOW_BUF_LEN
        {
            self.note_bad_window(info.run_id);
            event!(
                self.events,
                events::EVENT_GATEWAY_BOARD_ERROR,
                "window {} header inconsistent ({} bins, {} chunks)",
                info.run_id,
                info.n_bins,
                info.chunk_count
            );
            return None;
        }
        self.window_buf[..header.len()].copy_from_slice(header);
        let base = header.len();
        for chunk in 0..info.chunk_count {
            if chunk % 64 == 0 {
                wdog::alive();
            }
            let range = proto::field::chunk_range(chunk, data_len)?;
            let mut got = false;
            for _ in 0..3 {
                let nonce = board.next_nonce();
                let r = board
                    .transact(
                        Expect::chunk(Q_WINDOW_CHUNK, info.run_id, chunk),
                        Message::GetWindow(GetWindow {
                            command_nonce: nonce,
                            run_id: info.run_id,
                            chunk,
                        }),
                    )
                    .await;
                let c = match r.as_ref().ok().and_then(|r| r.find(proto::MSG_WINDOW_CHUNK)) {
                    Some(f) => match f.frame.message {
                        Message::WindowChunk(c) => c,
                        _ => continue,
                    },
                    None => {
                        if let Err(e) = r {
                            crate::board::log_err("GetWindow chunk", e);
                            if matches!(e, LinkError::Rejected(_)) {
                                return None;
                            }
                        }
                        continue;
                    }
                };
                // A late reply to an earlier chunk request is possible.
                if c.run_id != info.run_id || c.chunk != chunk || c.bytes().len() != range.len() {
                    continue;
                }
                self.window_buf[base + range.start..base + range.end].copy_from_slice(c.bytes());
                got = true;
                break;
            }
            if !got {
                info!("window {}: chunk {} failed", info.run_id, chunk);
                return None;
            }
        }
        if !info.verify_data(&self.window_buf[base..base + data_len]) {
            self.note_bad_window(info.run_id);
            event!(
                self.events,
                events::EVENT_GATEWAY_BOARD_ERROR,
                "window {} data_crc32 mismatch; left on board",
                info.run_id
            );
            return None;
        }
        Some((info, total))
    }

    async fn send_events(&mut self) {
        let mut sent: ArrayVec<i64, { events::QUEUE }> = ArrayVec::new();
        while let Some(ev) = self.events.front().cloned() {
            let t = self.clock.unix_at(ev.at).unwrap_or(0);
            let text = ev.text;
            let r = self
                .uplink
                .exchange(
                    2,
                    Duration::from_secs(5),
                    || {
                        Body::Event(up::Event {
                            t_unix_ms: t,
                            code: ev.code,
                            text: text.as_bytes(),
                        })
                    },
                    |r| matches!(r, Reply::EventAck(a) if a.t_unix_ms == t),
                )
                .await;
            if r.is_err() {
                info!("event {} not acknowledged; kept", ev.code);
                break;
            }
            let _ = sent.try_push(t);
            self.events.pop_front();
        }
        if !sent.is_empty() {
            info!("{} event(s) delivered", sent.len());
        }
    }

    // -----------------------------------------------------------------------
    // Sleep.

    fn finish(&mut self, server_ok: bool) -> u32 {
        if server_ok {
            self.fail_streak = 0;
        }
        let mut sleep = self.wake_interval_s;
        if let Some(post) = self.post_window_wake_s() {
            sleep = sleep.min(post);
        }
        if self.fail_streak > 0 {
            // 120, 240, 480, 960, 1920 s, then the normal interval.
            let backoff = 120u32 << (self.fail_streak - 1).min(5);
            sleep = sleep.min(backoff);
        }
        let sleep = sleep.max(MIN_SLEEP_S);
        info!(
            "cycle done: sleep {} s (interval {} s, fail streak {})",
            sleep, self.wake_interval_s, self.fail_streak
        );
        sleep
    }

    /// Seconds until 60 s after the board's next window ends, from its
    /// FieldStatus (board RTC); None if unknown or disabled.
    fn post_window_wake_s(&self) -> Option<u32> {
        let (fs, at) = self.board_frames.field_status()?;
        if fs.next_pulse_unix_s == 0 || fs.schedule_flags & proto::FIELD_STATUS_RTC_VALID == 0 {
            return None;
        }
        let recovery_s = self
            .server_schedule
            .filter(|s| s.crc16() == fs.schedule_crc)
            .map(|s| s.recovery_s)
            .unwrap_or(Schedule::FACTORY_DEFAULT.recovery_s) as i64;
        let board_now_ms = fs.rtc_unix_ms + at.elapsed().as_millis() as i64;
        let target_ms = (fs.next_pulse_unix_s as i64 + recovery_s + POST_WINDOW_MARGIN_S as i64) * 1000;
        let wait_s = (target_ms - board_now_ms) / 1000;
        Some(wait_s.clamp(MIN_SLEEP_S as i64, MAX_WAKE_INTERVAL_S as i64) as u32)
    }
}
