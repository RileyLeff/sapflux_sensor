//! UDP ingest: authenticate, replay-check, dispatch, reply.

use crate::config::Device;
use crate::store::{CheckinRecord, Store};
use sapflux_uplink::{
    self as up, Body, CheckInReply, EventAck, Header, MAX_DATAGRAM_LEN, REQUEST_REBOOT,
    REQUEST_SEND_WINDOWS,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;
use tracing::{debug, error, info, warn};

/// Why a datagram got no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drop {
    Malformed(up::Error),
    UnknownDevice(u64),
    BadMac(u64),
    Replay { device_id: u64, boot_id: u32, seq: u32 },
    NotARequest(u8),
    Internal(String),
}

/// Ingest state shared with the API.
pub struct Ingest {
    keys: HashMap<u64, Device>,
    store: Arc<Mutex<Store>>,
}

impl Ingest {
    /// New handler over a store.
    pub fn new(devices: Vec<Device>, store: Arc<Mutex<Store>>) -> Ingest {
        Ingest {
            keys: devices.into_iter().map(|d| (d.device_id, d)).collect(),
            store,
        }
    }

    /// Handle one datagram received at `t2_ms`; returns the sealed reply.
    pub fn handle(&self, dg: &[u8], peer: SocketAddr, t2_ms: i64) -> Result<Vec<u8>, Drop> {
        let header = up::peek_header(dg).map_err(Drop::Malformed)?;
        let dev = self
            .keys
            .get(&header.device_id)
            .ok_or(Drop::UnknownDevice(header.device_id))?;
        let (header, body) = up::open(dg, &dev.key).map_err(|e| match e {
            up::Error::BadMac => Drop::BadMac(header.device_id),
            e => Drop::Malformed(e),
        })?;
        if !matches!(
            header.msg_type,
            up::TYPE_CHECKIN | up::TYPE_WINDOW_PART | up::TYPE_EVENT
        ) {
            return Err(Drop::NotARequest(header.msg_type));
        }
        let store = self.store.lock().map_err(|_| Drop::Internal("store poisoned".into()))?;
        let internal = |e: anyhow::Error| Drop::Internal(format!("{e:#}"));
        if !store
            .accept_seq(header.device_id, header.boot_id, header.seq)
            .map_err(internal)?
        {
            return Err(Drop::Replay {
                device_id: header.device_id,
                boot_id: header.boot_id,
                seq: header.seq,
            });
        }
        let reply = match body {
            Body::CheckIn(c) => {
                let settings = store.settings(header.device_id).map_err(internal)?;
                let fs = store
                    .record_checkin(
                        &CheckinRecord {
                            device_id: header.device_id,
                            peer: peer.to_string(),
                            boot_id: header.boot_id,
                            seq: header.seq,
                            t1_ms: c.t1_unix_ms,
                            t2_ms,
                            gw_fw: c.gw_fw,
                            batt_mv: c.batt_mv,
                            rsrp_dbm: c.rsrp_dbm,
                            uptime_s: c.uptime_s,
                            flags: c.flags,
                            board_frames: c.board_frames,
                        },
                        &settings,
                    )
                    .map_err(internal)?;
                let reboot = store
                    .reboot_flag(header.device_id, header.boot_id)
                    .map_err(internal)?;
                let mut request_flags = 0;
                if settings.send_windows {
                    request_flags |= REQUEST_SEND_WINDOWS;
                }
                if reboot {
                    request_flags |= REQUEST_REBOOT;
                }
                info!(
                    device = %dev.name, %peer, boot_id = header.boot_id, seq = header.seq,
                    batt_mv = c.batt_mv, rsrp_dbm = c.rsrp_dbm,
                    t2_minus_t1_ms = t2_ms - c.t1_unix_ms,
                    board_frames = c.board_frames.len(),
                    windows_pending = fs.map(|f| f.windows_pending),
                    schedule_in_sync = fs.map(|f| f.schedule_crc == settings.schedule_crc),
                    enabled = settings.enabled, config_version = settings.config_version,
                    reboot, "check-in"
                );
                Body::CheckInReply(CheckInReply {
                    t1_unix_ms: c.t1_unix_ms,
                    t2_unix_ms: t2_ms,
                    t3_unix_ms: 0, // stamped just before sealing
                    config_version: settings.config_version,
                    schedule: settings.schedule(),
                    lease_s: settings.lease_s,
                    wake_interval_s: settings.wake_interval_s,
                    request_flags,
                })
            }
            Body::WindowPart(p) => {
                let ack = store.handle_part(header.device_id, &p).map_err(internal)?;
                debug!(
                    device = %dev.name, board_id = format!("{:016X}", p.board_id), run_id = p.run_id,
                    part = p.part, part_count = p.part_count, complete = ack.complete, "window part"
                );
                Body::WindowAck(ack)
            }
            Body::Event(e) => {
                let text = String::from_utf8_lossy(e.text);
                store
                    .record_event(
                        Some(header.device_id),
                        "gateway",
                        Some(header.boot_id),
                        Some(header.seq),
                        e.t_unix_ms,
                        e.code,
                        &text,
                    )
                    .map_err(internal)?;
                info!(device = %dev.name, code = e.code, text = %text, t_unix_ms = e.t_unix_ms, "event");
                Body::EventAck(EventAck {
                    t_unix_ms: e.t_unix_ms,
                })
            }
            _ => unreachable!("type filtered above"),
        };
        drop(store);
        Ok(seal_reply(&header, reply, &dev.key))
    }
}

fn seal_reply(req: &Header, mut body: Body<'_>, key: &[u8]) -> Vec<u8> {
    let reply_type = body.msg_type();
    if let Body::CheckInReply(r) = &mut body {
        r.t3_unix_ms = crate::now_ms();
    }
    let mut out = [0u8; MAX_DATAGRAM_LEN];
    let n = up::seal(&req.reply(reply_type), &body, key, &mut out).expect("replies fit");
    out[..n].to_vec()
}

/// Run the UDP loop until the socket fails.
pub async fn run(sock: UdpSocket, ingest: Arc<Ingest>) -> std::io::Result<()> {
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await?;
        let t2 = crate::now_ms();
        if n > MAX_DATAGRAM_LEN {
            warn!(%peer, len = n, "oversized datagram dropped");
            continue;
        }
        let dg = buf[..n].to_vec();
        let ing = ingest.clone();
        // SQLite work is synchronous; keep it off the reactor thread.
        let res = tokio::task::spawn_blocking(move || ing.handle(&dg, peer, t2)).await;
        match res {
            Ok(Ok(reply)) => {
                if let Err(e) = sock.send_to(&reply, peer).await {
                    warn!(%peer, error = %e, "send failed");
                }
            }
            Ok(Err(Drop::Internal(e))) => error!(%peer, error = %e, "internal error; datagram dropped"),
            Ok(Err(d @ Drop::Malformed(_))) => debug!(%peer, reason = ?d, "datagram dropped"),
            Ok(Err(d)) => warn!(%peer, reason = ?d, "datagram dropped"),
            Err(e) => error!(%peer, error = %e, "handler panicked"),
        }
    }
}
