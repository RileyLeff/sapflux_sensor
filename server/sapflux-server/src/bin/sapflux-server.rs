//! sap-flux UDP ingest + admin API.
//!
//! `sapflux-server --config /etc/sapflux/server.toml`

use anyhow::Result;
use clap::Parser;
use sapflux_server::{check_production, config::Config, init_logging, start};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "sap-flux UDP ingest server and admin API")]
struct Args {
    /// Config file (TOML, holds device keys).
    #[arg(long, short, env = "SAPFLUX_CONFIG", default_value = "/etc/sapflux/server.toml")]
    config: PathBuf,
    /// Allow the all-zero placeholder key (local testing only).
    #[arg(long)]
    allow_placeholder_keys: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = Config::load(&args.config)?;
    init_logging(&cfg.server.log_format);
    if !args.allow_placeholder_keys {
        check_production(&cfg)?;
    }
    let running = start(&cfg).await?;
    let mut udp = running.udp_task;
    let mut api = running.api_task;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // Exit nonzero if a task dies so systemd restarts us.
    tokio::select! {
        r = &mut udp => anyhow::bail!("UDP task ended: {r:?}"),
        r = &mut api => anyhow::bail!("API task ended: {r:?}"),
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupt, shutting down"),
        _ = term.recv() => tracing::info!("SIGTERM, shutting down"),
    }
    Ok(())
}
