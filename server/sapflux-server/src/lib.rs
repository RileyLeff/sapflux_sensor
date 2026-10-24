//! sap-flux field server: UDP ingest for the nRF9160 gateway
//! (`docs/field-protocol.md` section 2), SQLite storage, an admin HTTP API,
//! an admin CLI (`sapflux`) and a gateway simulator.

pub mod api;
pub mod board;
pub mod config;
pub mod ingest;
pub mod sim;
pub mod store;
pub mod window;

use anyhow::{bail, Context, Result};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// Wall clock, Unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Format Unix ms as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn fmt_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    )
}

/// Set up `tracing` (JSON lines or text) on stderr; `RUST_LOG` overrides
/// the default `info` level.
pub fn init_logging(format: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let b = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi);
    let _ = if format == "json" {
        b.json().flatten_event(true).with_current_span(false).try_init()
    } else {
        b.try_init()
    };
}

/// A running server.
pub struct Running {
    pub udp_addr: SocketAddr,
    pub api_addr: SocketAddr,
    pub store: Arc<Mutex<store::Store>>,
    pub udp_task: tokio::task::JoinHandle<std::io::Result<()>>,
    pub api_task: tokio::task::JoinHandle<std::io::Result<()>>,
}

/// Refuse obviously unsafe production configs.
pub fn check_production(cfg: &config::Config) -> Result<()> {
    for d in cfg.devices() {
        if d.key.iter().all(|&b| b == 0) {
            bail!(
                "device {:?} still has the all-zero placeholder key; run `sapflux keygen`",
                d.name
            );
        }
    }
    if cfg.devices.is_empty() {
        tracing::warn!("no [[device]] entries: every datagram will be dropped");
    }
    Ok(())
}

/// Open storage, bind both sockets and spawn the ingest and API tasks.
pub async fn start(cfg: &config::Config) -> Result<Running> {
    let store = store::Store::open(&cfg.server.data_dir)?;
    store.sync_devices(cfg)?;
    let store = Arc::new(Mutex::new(store));
    let sock = tokio::net::UdpSocket::bind(cfg.server.udp_bind)
        .await
        .with_context(|| format!("binding UDP {}", cfg.server.udp_bind))?;
    let udp_addr = sock.local_addr()?;
    let listener = tokio::net::TcpListener::bind(cfg.server.api_bind)
        .await
        .with_context(|| format!("binding API {}", cfg.server.api_bind))?;
    let api_addr = listener.local_addr()?;
    let ingest = Arc::new(ingest::Ingest::new(cfg.devices(), store.clone()));
    let udp_task = tokio::spawn(ingest::run(sock, ingest));
    let app = api::router(store.clone());
    let api_task = tokio::spawn(async move { axum::serve(listener, app).await });
    tracing::info!(%udp_addr, %api_addr, data_dir = %cfg.server.data_dir.display(),
                   devices = cfg.devices.len(), "sapflux-server listening");
    Ok(Running {
        udp_addr,
        api_addr,
        store,
        udp_task,
        api_task,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_format() {
        // 1787618352639 ms = 2026-08-25T00:39:12.639Z (computed with Python datetime).
        assert_eq!(super::fmt_utc(1_787_618_352_639), "2026-08-25T00:39:12.639Z");
        assert_eq!(super::fmt_utc(0), "1970-01-01T00:00:00.000Z");
    }
}
