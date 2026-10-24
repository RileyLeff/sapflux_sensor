//! Gateway simulator: plays the nRF9160 Feather against a server.
//!
//! sapflux-gateway-sim --server 127.0.0.1:47800 --device-id 352656100000001 \
//!     --key <64 hex> [--capture samples.csv] [--drop 2,5] [--skew-ms 5000]

use anyhow::{Context, Result};
use clap::Parser;
use sapflux_server::sim::{capture_window, synthetic_window, Gateway, SimBoard};
use sapflux_server::{board, config, fmt_utc};
use sapflux_uplink::{EVENT_BOOT, REQUEST_SEND_WINDOWS};
use std::net::ToSocketAddrs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Simulated sap-flux gateway (Feather) for end-to-end tests")]
struct Args {
    /// Server UDP address.
    #[arg(long, default_value = "127.0.0.1:47800")]
    server: String,
    /// Gateway device_id (decimal or 0x hex).
    #[arg(long)]
    device_id: String,
    /// 32-byte key, 64 hex digits.
    #[arg(long, env = "SAPFLUX_KEY_HEX")]
    key: String,
    /// Board id to report in Hello.
    #[arg(long, default_value = "0xD15C55B4116E105E")]
    board_id: String,
    /// Build the window from a sap-flux-monitor samples.csv instead of synthetic data.
    #[arg(long)]
    capture: Option<PathBuf>,
    /// Parts to drop on the first pass (comma separated) to exercise resends.
    #[arg(long, value_delimiter = ',')]
    drop: Vec<u16>,
    /// Gateway clock error in ms (time sync should measure -skew).
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    skew_ms: i64,
    /// Skip the window upload.
    #[arg(long)]
    no_window: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let server = a.server.to_socket_addrs()?.next().context("bad --server")?;
    let device_id = config::parse_u64(&a.device_id).context("bad --device-id")?;
    let board_id = config::parse_u64(&a.board_id).context("bad --board-id")?;
    let key = board::unhex(&a.key).context("bad --key")?;
    let boot_id = u32::from_le_bytes(sapflux_server::now_ms().to_le_bytes()[..4].try_into()?);
    let mut gw = Gateway::new(server, device_id, key, boot_id).await?;
    gw.clock_skew_ms = a.skew_ms;
    let mut brd = SimBoard::new(board_id);

    gw.event(EVENT_BOOT, "gateway-sim boot").await?;
    println!("event boot: acked");

    let c = gw.check_in(&mut brd).await?;
    println!(
        "check-in: offset {} ms, rtt {} ms, config v{}, enabled {}, request_flags 0x{:x}, pushed schedule {}",
        c.offset_ms, c.rtt_ms, c.reply.config_version, c.reply.schedule.enabled, c.reply.request_flags,
        c.pushed_schedule
    );
    println!("  server time {}", fmt_utc(c.reply.t3_unix_ms));

    if !a.no_window && c.reply.request_flags & REQUEST_SEND_WINDOWS != 0 {
        let (run_id, bytes) = match &a.capture {
            Some(p) => capture_window(p)?,
            None => {
                let run = (sapflux_server::now_ms() / 1000) as u32;
                let mut s = brd.schedule;
                s.enabled = 1;
                (run, synthetic_window(run, sapflux_server::now_ms() - 200_000, &s).0)
            }
        };
        let (ack, sent) = gw.send_window(board_id, run_id, &bytes, &a.drop, 5).await?;
        println!(
            "window run {run_id}: {} bytes, {} parts, {sent} sends, complete {}",
            bytes.len(), ack.part_count, ack.complete
        );
    }

    let c = gw.check_in(&mut brd).await?;
    println!(
        "check-in 2: offset {} ms, rtt {} ms, board schedule in sync: {}",
        c.offset_ms, c.rtt_ms, !c.pushed_schedule
    );
    Ok(())
}
