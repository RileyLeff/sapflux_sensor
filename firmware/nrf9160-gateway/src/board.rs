//! Board link: protocol v1 frames over the J6 TTL UART (115200 8N1).
//!
//! Feather RX = P0.23 (no pull: the board's PA14 has the 10 kΩ BOOT0
//! pull-down), Feather TX = P0.24. The board answers only when asked; it
//! leaves its TX undriven between replies, so the line idles low through the
//! pull-down and the UARTE sees break/framing noise and stray zero bytes.
//! Zero bytes are COBS delimiters, so they are harmless; a stray non-zero
//! byte in front of a frame is handled by retrying the COBS decode with the
//! first one or two bytes dropped.

use arrayvec::ArrayVec;
use embassy_nrf::buffered_uarte::BufferedUarte;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use sap_flux_protocol::{
    self as proto, DecodeError, Frame, MAX_ENCODED_LEN, MAX_FRAME_LEN, Message, cobs,
    decode_frame,
};

use crate::info;

/// Delay after the 0x00 wake byte (spec: at least 10 ms).
const WAKE_DELAY_MS: u64 = 12;
/// Retries after the first attempt (spec: up to 3).
pub const RETRIES: usize = 3;
/// Treat the board as possibly asleep again after this much idle time.
const IDLE_REWAKE_MS: u64 = 500;

/// Per-attempt reply timeouts. The spec promises a reply within 200 ms,
/// except while the heater is on (answered after the pulse, ≤ 1.5 s), so the
/// later attempts wait longer to ride out a pulse.
pub const TIMEOUTS_MS: [u64; RETRIES + 1] = [200, 300, 600, 1700];

/// A decoded frame plus its raw (un-stuffed) bytes, which the gateway
/// forwards to the server verbatim.
#[derive(Clone)]
pub struct RawFrame {
    buf: [u8; MAX_FRAME_LEN],
    len: usize,
    pub frame: Frame,
    pub at: Instant,
}

impl RawFrame {
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn msg_type(&self) -> u8 {
        self.frame.message.message_type()
    }
}

/// Streaming COBS accumulator that keeps the raw frame bytes.
struct Accumulator {
    buf: [u8; MAX_ENCODED_LEN],
    len: usize,
    overflow: bool,
}

impl Accumulator {
    const fn new() -> Self {
        Self {
            buf: [0; MAX_ENCODED_LEN],
            len: 0,
            overflow: false,
        }
    }

    fn reset(&mut self) {
        self.len = 0;
        self.overflow = false;
    }

    fn push(&mut self, byte: u8) -> Option<Result<RawFrame, DecodeError>> {
        if byte != 0 {
            if self.len < self.buf.len() {
                self.buf[self.len] = byte;
                self.len += 1;
            } else {
                self.overflow = true;
            }
            return None;
        }
        let len = self.len;
        let overflow = self.overflow;
        self.reset();
        if len == 0 {
            return None;
        }
        if overflow {
            return Some(Err(DecodeError::TooLong));
        }
        let mut first_err = DecodeError::Cobs;
        for skip in 0..3usize.min(len) {
            match try_decode(&self.buf[skip..len]) {
                Ok(frame) => return Some(Ok(frame)),
                Err(e) if skip == 0 => first_err = e,
                Err(_) => {}
            }
        }
        Some(Err(first_err))
    }
}

fn try_decode(stuffed: &[u8]) -> Result<RawFrame, DecodeError> {
    let mut buf = [0u8; MAX_FRAME_LEN];
    let n = match cobs::decode(stuffed, &mut buf) {
        Ok(n) => n,
        Err(cobs::CobsError::OutputOverflow) => return Err(DecodeError::TooLong),
        Err(cobs::CobsError::Malformed) => return Err(DecodeError::Cobs),
    };
    let frame = decode_frame(&buf[..n])?;
    Ok(RawFrame {
        buf,
        len: n,
        frame,
        at: Instant::now(),
    })
}

/// Frames collected in reply to one command.
pub struct Reply {
    pub frames: ArrayVec<RawFrame, 4>,
}

impl Reply {
    pub fn find(&self, msg_type: u8) -> Option<&RawFrame> {
        self.frames.iter().find(|f| f.msg_type() == msg_type)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// No complete reply after all retries.
    Timeout,
    /// The board answered with an `Ack` carrying a nonzero result.
    Rejected(u8),
    /// UART write failed.
    Uart,
}

/// What a command's reply must contain.
#[derive(Clone, Copy)]
pub struct Expect {
    /// Message types that must all be present.
    pub types: &'static [u8],
    /// An `Ack` with result 0 alone does not complete the reply (it does
    /// when `types` contains `MSG_ACK`); a nonzero `Ack` always does.
    pub timeouts_ms: &'static [u64],
    /// Only accept a `WindowChunk` for this `(run_id, chunk)`. Chunks carry
    /// no nonce, so a late reply to an earlier request must be skipped
    /// rather than completing (and wasting) this attempt.
    pub chunk: Option<(u32, u16)>,
}

impl Expect {
    pub const fn new(types: &'static [u8]) -> Self {
        Self {
            types,
            timeouts_ms: &TIMEOUTS_MS,
            chunk: None,
        }
    }

    pub const fn chunk(types: &'static [u8], run_id: u32, chunk: u16) -> Self {
        Self {
            chunk: Some((run_id, chunk)),
            ..Self::new(types)
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub commands: u32,
    pub retries: u32,
    pub failures: u32,
    pub decode_errors: u32,
}

pub struct BoardLink<'d> {
    uart: BufferedUarte<'d>,
    acc: Accumulator,
    seq: u32,
    nonce: u64,
    last_io: Option<Instant>,
    pub stats: Stats,
}

impl<'d> BoardLink<'d> {
    pub fn new(uart: BufferedUarte<'d>, nonce_seed: u64) -> Self {
        Self {
            uart,
            acc: Accumulator::new(),
            seq: 1,
            nonce: nonce_seed,
            last_io: None,
            stats: Stats::default(),
        }
    }

    /// Next command nonce.
    pub fn next_nonce(&mut self) -> u64 {
        self.nonce = self.nonce.wrapping_add(1);
        self.nonce
    }

    /// Discard anything already received.
    fn drain(&mut self) {
        loop {
            // A zero timeout polls the read once without waiting.
            let n = match embassy_futures_poll(self.uart.fill_buf()) {
                Some(Ok(data)) => data.len(),
                _ => 0,
            };
            if n == 0 {
                break;
            }
            self.uart.consume(n);
        }
        self.acc.reset();
    }

    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = self.uart.write(rest).await.map_err(|_| LinkError::Uart)?;
            rest = &rest[n..];
        }
        self.uart.flush().await.map_err(|_| LinkError::Uart)?;
        self.last_io = Some(Instant::now());
        Ok(())
    }

    /// Send the 0x00 wake byte, wait, and drop whatever came back.
    pub async fn wake(&mut self) -> Result<(), LinkError> {
        self.write_all(&[0x00]).await?;
        Timer::after_millis(WAKE_DELAY_MS).await;
        self.drain();
        Ok(())
    }

    async fn send(&mut self, msg: &Message) -> Result<usize, LinkError> {
        let mut out = [0u8; MAX_ENCODED_LEN];
        let n = proto::encode(msg, self.seq, &mut out).map_err(|_| LinkError::Uart)?;
        self.seq = self.seq.wrapping_add(1);
        self.write_all(&out[..n]).await?;
        Ok(n)
    }

    /// Send `msg` and collect its reply, retrying per the spec.
    ///
    /// `build` is called before every attempt so time-sensitive payloads
    /// (SetRtc) are stamped fresh; it receives the encoded length of the
    /// previous attempt (unused) and must return the same command with the
    /// same nonce.
    pub async fn transact_with(
        &mut self,
        expect: Expect,
        mut build: impl FnMut() -> Message,
    ) -> Result<Reply, LinkError> {
        self.stats.commands += 1;
        let mut last_rejection = None;
        for (attempt, &timeout_ms) in expect.timeouts_ms.iter().enumerate() {
            let idle = self
                .last_io
                .map(|t| t.elapsed() > Duration::from_millis(IDLE_REWAKE_MS))
                .unwrap_or(true);
            if attempt > 0 {
                self.stats.retries += 1;
            }
            if attempt > 0 || idle {
                self.wake().await?;
            } else {
                self.drain();
            }
            let msg = build();
            let cmd_type = msg.message_type();
            let nonce = command_nonce(&msg);
            self.send(&msg).await?;
            match self.collect(expect, cmd_type, nonce, timeout_ms).await {
                Ok(reply) => return Ok(reply),
                Err(Some(result)) => {
                    // An explicit rejection is final; do not retry.
                    last_rejection = Some(result);
                    break;
                }
                Err(None) => {}
            }
        }
        self.stats.failures += 1;
        match last_rejection {
            Some(r) => Err(LinkError::Rejected(r)),
            None => Err(LinkError::Timeout),
        }
    }

    pub async fn transact(&mut self, expect: Expect, msg: Message) -> Result<Reply, LinkError> {
        self.transact_with(expect, || msg).await
    }

    /// Ok(reply) when complete, Err(Some(code)) on a rejecting Ack,
    /// Err(None) on timeout.
    async fn collect(
        &mut self,
        expect: Expect,
        cmd_type: u8,
        nonce: u64,
        timeout_ms: u64,
    ) -> Result<Reply, Option<u8>> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut reply = Reply {
            frames: ArrayVec::new(),
        };
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(None);
            }
            let data = match with_timeout(deadline - now, self.uart.fill_buf()).await {
                Err(_) => return Err(None),
                Ok(Err(_)) => {
                    // Overrun: drop the partial frame and keep listening.
                    self.acc.reset();
                    continue;
                }
                Ok(Ok(data)) => data,
            };
            let mut chunk = [0u8; 64];
            let n = data.len().min(chunk.len());
            chunk[..n].copy_from_slice(&data[..n]);
            self.uart.consume(n);
            self.last_io = Some(Instant::now());

            for &b in &chunk[..n] {
                match self.acc.push(b) {
                    None => {}
                    Some(Err(_)) => self.stats.decode_errors += 1,
                    Some(Ok(frame)) => {
                        if let (Some((run_id, chunk)), Message::WindowChunk(c)) =
                            (expect.chunk, &frame.frame.message)
                        {
                            if c.run_id != run_id || c.chunk != chunk {
                                continue; // late reply to an earlier chunk request
                            }
                        }
                        if let Message::Ack(a) = frame.frame.message {
                            if a.command_type != cmd_type || a.command_nonce != nonce {
                                continue; // stale reply to an earlier command
                            }
                            if a.result != proto::ACK_OK {
                                return Err(Some(a.result));
                            }
                        }
                        let relevant = frame.msg_type() == proto::MSG_ACK
                            || expect.types.contains(&frame.msg_type());
                        if relevant && !reply.frames.is_full() {
                            // Replace an older frame of the same type.
                            if let Some(slot) =
                                reply.frames.iter_mut().find(|f| f.msg_type() == frame.msg_type())
                            {
                                *slot = frame;
                            } else {
                                reply.frames.push(frame);
                            }
                        }
                        if expect.types.iter().all(|t| reply.find(*t).is_some()) {
                            return Ok(reply);
                        }
                    }
                }
            }
        }
    }
}

fn command_nonce(msg: &Message) -> u64 {
    match msg {
        Message::Query(m) => m.command_nonce,
        Message::SetRtc(m) => m.command_nonce,
        Message::SetSchedule(m) => m.command_nonce,
        Message::GetWindow(m) => m.command_nonce,
        Message::AckWindow(m) => m.command_nonce,
        Message::GetFieldStatus(m) => m.command_nonce,
        Message::StartBaseline(m) => m.command_nonce,
        Message::ArmPulse(m) => m.command_nonce,
        Message::FirePulse(m) => m.command_nonce,
        Message::Abort(m) => m.command_nonce,
        _ => 0,
    }
}

/// Poll a future exactly once.
fn embassy_futures_poll<F: core::future::Future>(fut: F) -> Option<F::Output> {
    use core::pin::pin;
    use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: the vtable functions are no-ops and never dereference the
    // data pointer.
    let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    match pin!(fut).poll(&mut cx) {
        Poll::Ready(v) => Some(v),
        Poll::Pending => None,
    }
}

/// Log helper for link errors.
pub fn log_err(what: &str, e: LinkError) {
    info!("board: {} failed: {:?}", what, e);
}
