//! Request/response link to the board's field-mode command interface.
//!
//! Every command follows the gateway's wake discipline from
//! `docs/field-protocol.md` §1: write a single `0x00` wake byte, wait at
//! least 10 ms, send the command frame, then collect replies until the
//! expected set has arrived or the reply timeout expires. A command whose
//! replies time out is re-sent (same nonce) up to `retries` more times.
//! Rejections (`Ack` with a nonzero result) are never retried.

use sap_flux_protocol::{
    encode, Ack, AckWindow, Decoder, FieldStatus, GetFieldStatus, GetRaw, GetWindow, Hello,
    Message, Query, RawChunk, SetRtc, SetSchedule, Status, WindowChunk, WindowInfo, ACK_NOT_FOUND,
    CHUNK_HEADER, MAX_ENCODED_LEN, MSG_ACK_WINDOW, MSG_GET_RAW, MSG_GET_WINDOW, MSG_SET_RTC,
    MSG_SET_SCHEDULE,
};
use std::fmt;
use std::io;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Byte source/sink the link runs over (a serial port, or a fake board in
/// tests).
pub trait Transport {
    /// Write all bytes and wait until they have left the host.
    fn send(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Read whatever is available, waiting at most a short poll interval.
    /// Returns `Ok(0)` when nothing arrived.
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Discard unread input.
    fn clear_input(&mut self) -> io::Result<()>;
    /// Sleep (fakes may skip real sleeping).
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Link timing and retry policy.
#[derive(Debug, Clone, Copy)]
pub struct LinkOptions {
    /// Delay between the wake byte and the command frame (spec: ≥ 10 ms).
    pub wake_delay: Duration,
    /// How long to wait for a command's replies. The board answers within
    /// 200 ms normally and ≤ 1.5 s if the command lands during a pulse.
    pub timeout: Duration,
    /// Extra attempts after the first when replies time out (spec: 3).
    pub retries: u32,
    /// After the required replies arrive, how long to keep listening for
    /// optional trailing ones (e.g. FieldStatus after Query).
    pub linger: Duration,
    /// Print every frame sent and received to stderr.
    pub verbose: bool,
}

impl Default for LinkOptions {
    fn default() -> Self {
        Self {
            wake_delay: Duration::from_millis(15),
            timeout: Duration::from_millis(1800),
            retries: 3,
            linger: Duration::from_millis(150),
            verbose: false,
        }
    }
}

/// Link failure.
#[derive(Debug)]
pub enum LinkError {
    /// Transport I/O error.
    Io(io::Error),
    /// No complete reply after every attempt.
    Timeout {
        /// Command name.
        command: &'static str,
        /// Attempts made.
        attempts: u32,
    },
    /// The board answered `Ack` with `NOT_FOUND` (unknown run or chunk).
    NotFound,
    /// The board's reply was inconsistent with the request.
    Protocol(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Io(e) => write!(f, "serial I/O error: {e}"),
            LinkError::Timeout { command, attempts } => {
                write!(f, "{command}: no reply after {attempts} attempt(s)")
            }
            LinkError::NotFound => write!(f, "board replied NOT_FOUND (unknown run or chunk)"),
            LinkError::Protocol(s) => write!(f, "protocol error: {s}"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<io::Error> for LinkError {
    fn from(e: io::Error) -> Self {
        LinkError::Io(e)
    }
}

/// What a reply collector decided after seeing the replies so far.
enum Progress {
    /// Keep waiting.
    Pending,
    /// Required replies present; keep listening for `linger` for optional
    /// ones, then finish.
    Linger,
    /// Complete.
    Done,
    /// Terminal failure (not retried).
    Fail(LinkError),
}

/// Replies to `Query`: `Ack`, `Hello`, `Status` (+ `FieldStatus` in field
/// mode).
#[derive(Debug, Clone, Copy)]
pub struct QueryReply {
    /// Board identification.
    pub hello: Hello,
    /// Board state.
    pub status: Status,
    /// Present only in field mode.
    pub field_status: Option<FieldStatus>,
    /// Host Unix ms when the command was sent.
    pub sent_unix_ms: i64,
    /// Host Unix ms when the Status reply was decoded.
    pub status_rx_unix_ms: i64,
}

/// Reply to a command answered by `Ack` (+ optional trailing frames).
#[derive(Debug, Clone, Copy)]
pub struct AckReply {
    /// The acknowledgement.
    pub ack: Ack,
    /// Trailing FieldStatus, if any (SetSchedule).
    pub field_status: Option<FieldStatus>,
    /// Trailing Status, if any (SetRtc).
    pub status: Option<Status>,
}

/// Host clock as Unix milliseconds.
pub fn host_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Board→host message types; anything else seen on the wire (e.g. an
/// RS-485 adapter echoing our own transmission) is ignored.
fn is_board_message(m: &Message) -> bool {
    matches!(
        m,
        Message::Hello(_)
            | Message::Sample(_)
            | Message::Status(_)
            | Message::Ack(_)
            | Message::Error(_)
            | Message::Sample12(_)
            | Message::WindowInfo(_)
            | Message::WindowChunk(_)
            | Message::FieldStatus(_)
            | Message::RawChunk(_)
    )
}

/// Command link over a [`Transport`].
pub struct Link<T: Transport> {
    transport: T,
    decoder: Decoder,
    sequence: u32,
    nonce: u64,
    /// Timing and retry policy.
    pub options: LinkOptions,
    /// Frames transmitted (attempts included).
    pub frames_sent: u64,
    /// Attempts that timed out and were retried.
    pub retries_used: u64,
}

impl<T: Transport> Link<T> {
    /// Wrap a transport.
    pub fn new(transport: T, options: LinkOptions) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self {
            transport,
            decoder: Decoder::new(),
            sequence: 0,
            nonce,
            options,
            frames_sent: 0,
            retries_used: 0,
        }
    }

    /// Access the transport (tests).
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    fn next_nonce(&mut self) -> u64 {
        self.nonce = self.nonce.wrapping_add(1);
        self.nonce
    }

    fn log(&self, s: impl FnOnce() -> String) {
        if self.options.verbose {
            eprintln!("{}", s());
        }
    }

    /// Wake, send one frame, and collect replies until `judge` says done.
    /// `build` is called per attempt (so time-stamped commands can be
    /// re-stamped); the nonce stays the same across attempts.
    fn transact(
        &mut self,
        name: &'static str,
        nonce: u64,
        mut build: impl FnMut(u64) -> Message,
        mut judge: impl FnMut(&[(i64, Message)]) -> Progress,
    ) -> Result<Vec<(i64, Message)>, LinkError> {
        let attempts = self.options.retries + 1;
        for attempt in 1..=attempts {
            if attempt > 1 {
                self.retries_used += 1;
                self.log(|| format!("{name}: retry {}/{}", attempt - 1, self.options.retries));
            }
            // Fresh receive state: stale bytes belong to nothing we asked.
            self.transport.clear_input()?;
            self.decoder.reset();

            self.transport.send(&[0x00])?;
            self.transport.sleep(self.options.wake_delay);

            let msg = build(nonce);
            let mut buf = [0u8; MAX_ENCODED_LEN];
            let n = encode(&msg, self.sequence, &mut buf)
                .map_err(|e| LinkError::Protocol(format!("encode: {e:?}")))?;
            self.sequence = self.sequence.wrapping_add(1);
            self.log(|| format!("-> {msg:?}"));
            self.transport.send(&buf[..n])?;
            self.frames_sent += 1;

            let mut replies: Vec<(i64, Message)> = Vec::new();
            let deadline = Instant::now() + self.options.timeout;
            let mut linger_until: Option<Instant> = None;
            let mut rx = [0u8; 512];
            loop {
                let now = Instant::now();
                if let Some(t) = linger_until {
                    if now >= t {
                        return Ok(replies);
                    }
                }
                if now >= deadline {
                    if linger_until.is_some() {
                        return Ok(replies);
                    }
                    break;
                }
                let got = self.transport.recv(&mut rx)?;
                let mut changed = false;
                for &b in &rx[..got] {
                    match self.decoder.push(b) {
                        Some(Ok(frame)) if is_board_message(&frame.message) => {
                            self.log(|| format!("<- {:?}", frame.message));
                            replies.push((host_unix_ms(), frame.message));
                            changed = true;
                        }
                        Some(Ok(frame)) => {
                            self.log(|| format!("<- (ignored echo) {:?}", frame.message));
                        }
                        Some(Err(e)) => self.log(|| format!("<- decode error {e:?}")),
                        None => {}
                    }
                }
                if changed {
                    match judge(&replies) {
                        Progress::Pending => {}
                        Progress::Linger => {
                            if linger_until.is_none() {
                                linger_until = Some(Instant::now() + self.options.linger);
                            }
                        }
                        Progress::Done => return Ok(replies),
                        Progress::Fail(e) => return Err(e),
                    }
                }
            }
            self.log(|| format!("{name}: attempt {attempt} timed out"));
        }
        Err(LinkError::Timeout {
            command: name,
            attempts,
        })
    }

    /// `Query` → `Ack` + `Hello` + `Status` (+ `FieldStatus`).
    pub fn query(&mut self) -> Result<QueryReply, LinkError> {
        let mut sent_unix_ms = 0;
        let nonce = self.next_nonce();
        let replies = self.transact(
            "Query",
            nonce,
            |nonce| {
                sent_unix_ms = host_unix_ms();
                Message::Query(Query {
                    command_nonce: nonce,
                })
            },
            |r| {
                let hello = r.iter().any(|(_, m)| matches!(m, Message::Hello(_)));
                let status = r.iter().any(|(_, m)| matches!(m, Message::Status(_)));
                let field = r.iter().any(|(_, m)| matches!(m, Message::FieldStatus(_)));
                match (hello && status, field) {
                    (true, true) => Progress::Done,
                    (true, false) => Progress::Linger,
                    _ => Progress::Pending,
                }
            },
        )?;
        let mut hello = None;
        let mut status = None;
        let mut field_status = None;
        for (t, m) in replies {
            match m {
                Message::Hello(h) => hello = Some(h),
                Message::Status(s) => status = Some((t, s)),
                Message::FieldStatus(f) => field_status = Some(f),
                _ => {}
            }
        }
        let (status_rx_unix_ms, status) = status.expect("judge requires Status");
        Ok(QueryReply {
            hello: hello.expect("judge requires Hello"),
            status,
            field_status,
            sent_unix_ms,
            status_rx_unix_ms,
        })
    }

    /// `GetFieldStatus` → `FieldStatus`. Also returns host send/receive
    /// Unix ms for offset estimates.
    pub fn field_status(&mut self) -> Result<(FieldStatus, i64, i64), LinkError> {
        let mut sent = 0;
        let nonce = self.next_nonce();
        let replies = self.transact(
            "GetFieldStatus",
            nonce,
            |nonce| {
                sent = host_unix_ms();
                Message::GetFieldStatus(GetFieldStatus {
                    command_nonce: nonce,
                })
            },
            |r| {
                if r.iter().any(|(_, m)| matches!(m, Message::FieldStatus(_))) {
                    Progress::Done
                } else {
                    Progress::Pending
                }
            },
        )?;
        replies
            .into_iter()
            .find_map(|(t, m)| match m {
                Message::FieldStatus(f) => Some((f, sent, t)),
                _ => None,
            })
            .ok_or_else(|| LinkError::Protocol("no FieldStatus".into()))
    }

    /// Generic Ack-answered command. `want_field_status` / `want_status`
    /// linger for a trailing frame after an accepting Ack.
    fn ack_command(
        &mut self,
        name: &'static str,
        command_type: u8,
        mut build: impl FnMut(u64) -> Message,
        want_field_status: bool,
        want_status: bool,
    ) -> Result<AckReply, LinkError> {
        let nonce = self.next_nonce();
        let is_our_ack = move |a: &Ack| a.command_type == command_type && a.command_nonce == nonce;
        let replies = self.transact(name, nonce, &mut build, |r| {
            let ack = r.iter().find_map(|(_, m)| match m {
                Message::Ack(a) if is_our_ack(a) => Some(*a),
                _ => None,
            });
            let Some(ack) = ack else {
                return Progress::Pending;
            };
            if ack.result != 0 {
                // Rejections may still be followed by FieldStatus;
                // linger briefly but never retry.
                return Progress::Linger;
            }
            let fs = r.iter().any(|(_, m)| matches!(m, Message::FieldStatus(_)));
            let st = r.iter().any(|(_, m)| matches!(m, Message::Status(_)));
            if (!want_field_status || fs) && (!want_status || st) {
                Progress::Done
            } else {
                Progress::Linger
            }
        })?;
        let mut out = AckReply {
            ack: Ack {
                command_type,
                result: 0,
                command_nonce: 0,
                run_id: 0,
            },
            field_status: None,
            status: None,
        };
        let mut have_ack = false;
        for (_, m) in replies {
            match m {
                Message::Ack(a) if is_our_ack(&a) && !have_ack => {
                    out.ack = a;
                    have_ack = true;
                }
                Message::FieldStatus(f) => out.field_status = Some(f),
                Message::Status(s) => out.status = Some(s),
                _ => {}
            }
        }
        if !have_ack {
            return Err(LinkError::Protocol(format!("{name}: no matching Ack")));
        }
        Ok(out)
    }

    /// `SetSchedule` → `Ack` then `FieldStatus`. `nonce` in `cmd` is
    /// replaced.
    pub fn set_schedule(&mut self, cmd: SetSchedule) -> Result<AckReply, LinkError> {
        self.ack_command(
            "SetSchedule",
            MSG_SET_SCHEDULE,
            |nonce| {
                Message::SetSchedule(SetSchedule {
                    command_nonce: nonce,
                    ..cmd
                })
            },
            true,
            false,
        )
    }

    /// `SetRtc` stamped from the host clock at the moment of each attempt
    /// (plus `lead_ms` for the frame's own transmit time) → `Ack` (+
    /// `Status`). Returns the reply and the `unix_ms` actually sent.
    pub fn set_rtc_now(&mut self, lead_ms: i64) -> Result<(AckReply, i64), LinkError> {
        let mut sent_ms = 0i64;
        let reply = self.ack_command(
            "SetRtc",
            MSG_SET_RTC,
            |nonce| {
                sent_ms = host_unix_ms() + lead_ms;
                Message::SetRtc(SetRtc {
                    command_nonce: nonce,
                    unix_ms: sent_ms,
                })
            },
            false,
            true,
        )?;
        Ok((reply, sent_ms))
    }

    /// `GetWindow` header → `WindowInfo`. `run_id` 0 = oldest undelivered.
    pub fn window_info(&mut self, run_id: u32) -> Result<WindowInfo, LinkError> {
        let nonce = self.next_nonce();
        let replies = self.transact(
            "GetWindow(header)",
            nonce,
            |nonce| {
                Message::GetWindow(GetWindow {
                    command_nonce: nonce,
                    run_id,
                    chunk: CHUNK_HEADER,
                })
            },
            |r| {
                judge_window(
                    r,
                    nonce,
                    |m| matches!(m, Message::WindowInfo(w) if run_id == 0 || w.run_id == run_id),
                )
            },
        )?;
        replies
            .into_iter()
            .find_map(|(_, m)| match m {
                Message::WindowInfo(w) if run_id == 0 || w.run_id == run_id => Some(w),
                _ => None,
            })
            .ok_or_else(|| LinkError::Protocol("no WindowInfo".into()))
    }

    /// `GetWindow` chunk → `WindowChunk`.
    pub fn window_chunk(&mut self, run_id: u32, chunk: u16) -> Result<WindowChunk, LinkError> {
        let is_ours = move |m: &Message| matches!(m, Message::WindowChunk(c) if c.run_id == run_id && c.chunk == chunk);
        let nonce = self.next_nonce();
        let replies = self.transact(
            "GetWindow(chunk)",
            nonce,
            |nonce| {
                Message::GetWindow(GetWindow {
                    command_nonce: nonce,
                    run_id,
                    chunk,
                })
            },
            |r| judge_window(r, nonce, is_ours),
        )?;
        replies
            .into_iter()
            .find_map(|(_, m)| match m {
                Message::WindowChunk(c) if c.run_id == run_id && c.chunk == chunk => Some(c),
                _ => None,
            })
            .ok_or_else(|| LinkError::Protocol("no WindowChunk".into()))
    }

    /// `GetRaw` → `RawChunk` (bytes `100·chunk ..` of the run's raw block
    /// stream). NOT_FOUND: unknown run, or past its end.
    pub fn raw_chunk(&mut self, run_id: u32, chunk: u32) -> Result<RawChunk, LinkError> {
        let is_ours = move |m: &Message| matches!(m, Message::RawChunk(c) if c.run_id == run_id && c.chunk == chunk);
        let nonce = self.next_nonce();
        let replies = self.transact(
            "GetRaw",
            nonce,
            |nonce| {
                Message::GetRaw(GetRaw {
                    command_nonce: nonce,
                    run_id,
                    chunk,
                })
            },
            |r| judge_reply(r, MSG_GET_RAW, nonce, is_ours),
        )?;
        replies
            .into_iter()
            .find_map(|(_, m)| match m {
                Message::RawChunk(c) if c.run_id == run_id && c.chunk == chunk => Some(c),
                _ => None,
            })
            .ok_or_else(|| LinkError::Protocol("no RawChunk".into()))
    }

    /// `AckWindow` → `Ack`.
    pub fn ack_window(&mut self, run_id: u32) -> Result<AckReply, LinkError> {
        self.ack_command(
            "AckWindow",
            MSG_ACK_WINDOW,
            |nonce| {
                Message::AckWindow(AckWindow {
                    command_nonce: nonce,
                    run_id,
                })
            },
            false,
            false,
        )
    }
}

/// GetWindow replies: the wanted frame, or a NOT_FOUND Ack.
fn judge_window(r: &[(i64, Message)], nonce: u64, wanted: impl Fn(&Message) -> bool) -> Progress {
    judge_reply(r, MSG_GET_WINDOW, nonce, wanted)
}

/// Data-fetch replies (GetWindow, GetRaw): the wanted frame, or an Ack for
/// this command (NOT_FOUND, or another rejection).
fn judge_reply(
    r: &[(i64, Message)],
    command_type: u8,
    nonce: u64,
    wanted: impl Fn(&Message) -> bool,
) -> Progress {
    for (_, m) in r {
        if wanted(m) {
            return Progress::Done;
        }
        if let Message::Ack(a) = m {
            if a.command_type == command_type && a.command_nonce == nonce {
                return if a.result == ACK_NOT_FOUND {
                    Progress::Fail(LinkError::NotFound)
                } else {
                    Progress::Fail(LinkError::Protocol(format!(
                        "command {command_type:#04x} rejected with result {:#04x}",
                        a.result
                    )))
                };
            }
        }
    }
    Progress::Pending
}
