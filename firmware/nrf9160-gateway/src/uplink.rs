//! UDP client for the server (docs/field-protocol.md section 2), using the
//! shared `sapflux-uplink` codec and nrf-modem's UDP socket.
//!
//! Every datagram gets a fresh `seq` (starting at 1 per boot), including
//! retries, so the server's replay check (`seq` ≤ last seen → drop) never
//! discards a retry. A reply is accepted if its `seq` is any of the seqs
//! sent for the current exchange.

use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use arrayvec::ArrayVec;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use nrf_modem::UdpSocket;
use sapflux_uplink::{
    self as up, Body, CheckInReply, EventAck, Header, MAX_DATAGRAM_LEN, MAX_PART_DATA, WindowAck,
};

use crate::config::{KEY, SERVER_IP, SERVER_PORT};
use crate::info;

/// Owned replies (no borrows into the receive buffer).
#[derive(Clone, Copy)]
pub enum Reply {
    CheckIn(CheckInReply, Instant),
    WindowAck(WindowAck),
    EventAck(EventAck),
}

#[derive(Debug)]
#[allow(dead_code)] // fields are for Debug output
pub enum UplinkError {
    Modem(nrf_modem::Error),
    Codec(up::Error),
    Timeout,
}

impl From<nrf_modem::Error> for UplinkError {
    fn from(e: nrf_modem::Error) -> Self {
        Self::Modem(e)
    }
}

pub struct Uplink {
    sock: Option<UdpSocket>,
    device_id: u64,
    boot_id: u32,
    seq: u32,
    tx: [u8; MAX_DATAGRAM_LEN],
    rx: [u8; MAX_DATAGRAM_LEN],
    sent: ArrayVec<u32, 16>,
    pub datagrams_sent: u32,
    pub bad_replies: u32,
}

fn server() -> SocketAddr {
    let [a, b, c, d] = SERVER_IP;
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), SERVER_PORT))
}

impl Uplink {
    pub const fn new(device_id: u64, boot_id: u32) -> Self {
        Self {
            sock: None,
            device_id,
            boot_id,
            seq: 0,
            tx: [0; MAX_DATAGRAM_LEN],
            rx: [0; MAX_DATAGRAM_LEN],
            sent: ArrayVec::new_const(),
            datagrams_sent: 0,
            bad_replies: 0,
        }
    }

    /// Close the socket (it is reopened on the next send). Used after a
    /// socket error.
    pub fn reset_socket(&mut self) {
        self.sock = None;
    }

    async fn ensure_socket(&mut self) -> Result<(), UplinkError> {
        if self.sock.is_none() {
            let any = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
            self.sock = Some(UdpSocket::bind(any).await?);
        }
        Ok(())
    }

    /// Begin a new request/response exchange.
    fn begin(&mut self) {
        self.sent.clear();
    }

    /// Seal and send one datagram; `body` is built with the fresh seq.
    async fn send(&mut self, body: &Body<'_>) -> Result<u32, UplinkError> {
        self.seq = self.seq.wrapping_add(1);
        let header = Header {
            msg_type: body.msg_type(),
            device_id: self.device_id,
            boot_id: self.boot_id,
            seq: self.seq,
        };
        let n = up::seal(&header, body, &KEY, &mut self.tx).map_err(UplinkError::Codec)?;
        if self.sent.is_full() {
            self.sent.remove(0);
        }
        self.sent.push(self.seq);
        self.ensure_socket().await?;
        let sock = self.sock.as_ref().unwrap();
        let r = sock.send_to(&self.tx[..n], server()).await;
        if let Err(e) = r {
            self.sock = None;
            return Err(UplinkError::Modem(e));
        }
        self.datagrams_sent += 1;
        Ok(self.seq)
    }

    /// Wait up to `timeout` for a valid reply to this exchange.
    async fn recv(&mut self, timeout: Duration) -> Result<Reply, UplinkError> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(UplinkError::Timeout);
            }
            if self.sock.is_none() {
                return Err(UplinkError::Timeout);
            }
            let sock = self.sock.as_ref().unwrap();
            let r = with_timeout(deadline - now, sock.receive_from(&mut self.rx)).await;
            let t4 = Instant::now();
            let (data, from) = match r {
                Err(_) => return Err(UplinkError::Timeout),
                Ok(Err(e)) => {
                    self.sock = None;
                    return Err(UplinkError::Modem(e));
                }
                Ok(Ok(v)) => v,
            };
            if from != server() {
                self.bad_replies += 1;
                continue;
            }
            let (h, body) = match up::open(data, &KEY) {
                Ok(v) => v,
                Err(e) => {
                    info!("uplink: bad datagram from server: {:?}", e);
                    self.bad_replies += 1;
                    continue;
                }
            };
            if h.device_id != self.device_id || h.boot_id != self.boot_id || !self.sent.contains(&h.seq)
            {
                self.bad_replies += 1;
                continue;
            }
            return Ok(match body {
                Body::CheckInReply(r) => Reply::CheckIn(r, t4),
                Body::WindowAck(a) => Reply::WindowAck(a),
                Body::EventAck(a) => Reply::EventAck(a),
                _ => {
                    self.bad_replies += 1;
                    continue;
                }
            });
        }
    }

    /// Send `build()` up to `attempts` times, waiting `timeout` for a reply
    /// that `accept` takes. `build` is called per attempt (fresh t1).
    pub async fn exchange<'b>(
        &mut self,
        attempts: u32,
        timeout: Duration,
        mut build: impl FnMut() -> Body<'b>,
        accept: impl Fn(&Reply) -> bool,
    ) -> Result<Reply, UplinkError> {
        self.begin();
        let mut last = UplinkError::Timeout;
        for attempt in 0..attempts {
            if attempt > 0 {
                Timer::after_millis(500).await;
            }
            let body = build();
            if let Err(e) = self.send(&body).await {
                info!("uplink: send failed: {:?}", e);
                last = e;
                continue;
            }
            loop {
                match self.recv(timeout).await {
                    Ok(r) if accept(&r) => return Ok(r),
                    Ok(_) => continue, // reply to an earlier exchange type
                    Err(e) => {
                        last = e;
                        break;
                    }
                }
            }
        }
        Err(last)
    }

    /// Upload one window (`data` = WindowInfo frame + window data) as
    /// `WindowPart`s until the server reports `complete`. The server answers
    /// each part with a WindowAck (bitmap LSB-first: part `i` is bit `i % 8`
    /// of byte `i / 8`). Parts are sent back to back with ~30 ms pacing
    /// while acks are drained; missing parts are resent for up to `rounds`
    /// rounds.
    pub async fn upload_window(
        &mut self,
        board_id: u64,
        run_id: u32,
        data: &[u8],
        rounds: u32,
    ) -> Result<(), UplinkError> {
        let total_len = data.len() as u32;
        let part_count = data.len().div_ceil(MAX_PART_DATA) as u16;
        if part_count == 0 || part_count > up::MAX_PARTS {
            return Err(UplinkError::Codec(up::Error::TooLong));
        }
        let mut have = [0u8; 32];
        let is_have = |have: &[u8; 32], i: u16| have[i as usize / 8] & (1 << (i % 8)) != 0;
        for round in 0..rounds {
            crate::wdog::alive();
            self.begin();
            let mut sent_any = false;
            for part in 0..part_count {
                if is_have(&have, part) {
                    continue;
                }
                let start = part as usize * MAX_PART_DATA;
                let end = (start + MAX_PART_DATA).min(data.len());
                let body = Body::WindowPart(up::WindowPart {
                    board_id,
                    run_id,
                    part,
                    part_count,
                    total_len,
                    data: &data[start..end],
                });
                match self.send(&body).await {
                    Ok(_) => sent_any = true,
                    Err(e) => {
                        info!("uplink: part {} send failed: {:?}", part, e);
                        Timer::after_millis(500).await;
                        continue;
                    }
                }
                // Pace and drain acks that arrived meanwhile.
                if let Some(done) = self
                    .drain_acks(board_id, run_id, &mut have, Duration::from_millis(30))
                    .await
                {
                    if done {
                        return Ok(());
                    }
                }
            }
            if !sent_any && round > 0 {
                Timer::after_millis(500).await;
            }
            // Wait for the last acks.
            if self
                .drain_acks(board_id, run_id, &mut have, Duration::from_secs(4))
                .await
                == Some(true)
            {
                return Ok(());
            }
            let missing = (0..part_count).filter(|&i| !is_have(&have, i)).count();
            if missing == 0 {
                // Every part stored but not complete: the server rejected
                // the reassembled window and cleared it; resend all.
                have = [0; 32];
            }
            info!(
                "uplink: window {} round {}: {} of {} parts missing",
                run_id,
                round + 1,
                missing,
                part_count
            );
        }
        Err(UplinkError::Timeout)
    }

    /// Collect WindowAcks for (board_id, run_id) until `quiet` passes with
    /// nothing new. Some(true) when complete.
    async fn drain_acks(
        &mut self,
        board_id: u64,
        run_id: u32,
        have: &mut [u8; 32],
        quiet: Duration,
    ) -> Option<bool> {
        let mut any = None;
        loop {
            match self.recv(quiet).await {
                Ok(Reply::WindowAck(a)) if a.board_id == board_id && a.run_id == run_id => {
                    // The server's bitmap is cumulative, and it clears it
                    // when reassembly fails verification, so the latest ack
                    // replaces (not ORs into) what we know.
                    *have = a.received_bitmap;
                    if a.complete {
                        return Some(true);
                    }
                    any = Some(false);
                }
                Ok(_) => {}
                Err(_) => return any,
            }
        }
    }
}
