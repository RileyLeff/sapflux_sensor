//! Server configuration (TOML, kept outside git: it holds device keys).

use anyhow::{bail, Context, Result};
use sapflux_uplink::Schedule;
use sap_flux_protocol::{ProbeKind, SCHEDULE_FLAG_SFK1};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Whole config file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "device")]
    pub devices: Vec<DeviceConfig>,
}

/// `[server]`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// UDP ingest bind address (public).
    #[serde(default = "default_udp")]
    pub udp_bind: SocketAddr,
    /// Admin HTTP API bind address (localhost or tailnet IP only; no auth).
    #[serde(default = "default_api")]
    pub api_bind: SocketAddr,
    /// SQLite database and window files live here.
    pub data_dir: PathBuf,
    /// `json` or `text`.
    #[serde(default = "default_log_format")]
    pub log_format: String,
}

fn default_udp() -> SocketAddr {
    "0.0.0.0:47800".parse().unwrap()
}
fn default_api() -> SocketAddr {
    "127.0.0.1:8470".parse().unwrap()
}
fn default_log_format() -> String {
    "text".into()
}

/// `[defaults]`: config a device gets the first time the server sees it in
/// this file. After that the database is authoritative (change it with the
/// CLI or API).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    pub enabled: bool,
    pub require_lease: bool,
    pub pulse_duration_ms: u16,
    pub interval_s: u32,
    pub phase_s: u32,
    pub baseline_s: u16,
    pub recovery_s: u16,
    pub sample_hz: u16,
    pub bin_ms: u16,
    /// Windows as SFK1 records (needs a probe kind) instead of bins.
    pub sfk1: bool,
    pub probe1: ProbeType,
    pub probe2: ProbeType,
    /// Installed upside down (heater-side needle order reversed). The board
    /// doesn't care; window files record it for the fit.
    pub probe1_flipped: bool,
    pub probe2_flipped: bool,
    pub lease_s: u32,
    pub wake_interval_s: u32,
    pub send_windows: bool,
}

/// Probe on a board port. It picks the SFK1 profile the board encodes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum ProbeType {
    #[default]
    None,
    /// East30, both needles 6 mm from the heater.
    East30,
    /// East30 offset probe, needles 10 mm and 5 mm from the heater.
    East30Offset,
}

impl ProbeType {
    /// The wire kind.
    pub fn kind(self) -> ProbeKind {
        match self {
            ProbeType::None => ProbeKind::None,
            ProbeType::East30 => ProbeKind::East30,
            ProbeType::East30Offset => ProbeKind::East30Offset,
        }
    }

    /// From the wire kind.
    pub fn from_kind(k: ProbeKind) -> Self {
        match k {
            ProbeKind::None => ProbeType::None,
            ProbeKind::East30 => ProbeType::East30,
            ProbeKind::East30Offset => ProbeType::East30Offset,
        }
    }
}

/// Schedule `flags` from the server's settings.
pub fn schedule_flags(require_lease: bool, sfk1: bool, probes: [ProbeType; 2]) -> u8 {
    let mut s = Schedule {
        flags: if require_lease { sapflux_uplink::SCHEDULE_FLAG_REQUIRE_LEASE } else { 0 }
            | if sfk1 { SCHEDULE_FLAG_SFK1 } else { 0 },
        ..Schedule::FACTORY_DEFAULT
    };
    for (port, p) in probes.into_iter().enumerate() {
        s = s.with_probe(port, p.kind());
    }
    s.flags
}

impl Default for Defaults {
    fn default() -> Self {
        let s = Schedule::FACTORY_DEFAULT;
        Defaults {
            enabled: false,
            require_lease: false,
            pulse_duration_ms: s.pulse_duration_ms,
            interval_s: s.interval_s,
            phase_s: s.phase_s,
            baseline_s: s.baseline_s,
            recovery_s: s.recovery_s,
            sample_hz: s.sample_hz,
            bin_ms: s.bin_ms,
            sfk1: false,
            probe1: ProbeType::None,
            probe2: ProbeType::None,
            probe1_flipped: false,
            probe2_flipped: false,
            lease_s: 3 * 3600,
            wake_interval_s: 3600,
            send_windows: true,
        }
    }
}

impl Defaults {
    /// The schedule these defaults describe.
    pub fn schedule(&self) -> Schedule {
        Schedule {
            enabled: self.enabled as u8,
            flags: schedule_flags(self.require_lease, self.sfk1, [self.probe1, self.probe2]),
            pulse_duration_ms: self.pulse_duration_ms,
            interval_s: self.interval_s,
            phase_s: self.phase_s,
            baseline_s: self.baseline_s,
            recovery_s: self.recovery_s,
            sample_hz: self.sample_hz,
            bin_ms: self.bin_ms,
        }
    }
}

/// `[[device]]`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// Short name used by the CLI/API.
    pub name: String,
    /// Gateway id (the u64 in every datagram, derived from the IMEI).
    /// Integer, or a string in decimal or `0x` hex.
    pub device_id: DeviceId,
    /// 32-byte HMAC key, 64 hex digits.
    pub key: String,
}

/// u64 device id from a TOML integer or string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceId(pub u64);

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            I(i64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::I(i) if i >= 0 => Ok(DeviceId(i as u64)),
            Raw::I(_) => Err(serde::de::Error::custom("device_id must be >= 0")),
            Raw::S(s) => parse_u64(&s)
                .map(DeviceId)
                .ok_or_else(|| serde::de::Error::custom(format!("bad device_id {s:?}"))),
        }
    }
}

/// Parse decimal or `0x` hex.
pub fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// A device with its decoded key.
#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,
    pub device_id: u64,
    pub key: Vec<u8>,
}

impl Config {
    /// Load and validate.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse from a string (tests).
    pub fn parse(text: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        sapflux_uplink::validate_schedule(&self.defaults.schedule())
            .map_err(|e| anyhow::anyhow!("[defaults]: {}", sapflux_uplink::schedule_error_text(e)))?;
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for d in &self.devices {
            if !names.insert(d.name.as_str()) {
                bail!("duplicate device name {:?}", d.name);
            }
            if !ids.insert(d.device_id.0) {
                bail!("duplicate device_id {}", d.device_id.0);
            }
            if parse_u64(&d.name).is_some() {
                bail!("device name {:?} must not be a number", d.name);
            }
            let key = crate::board::unhex(&d.key)
                .with_context(|| format!("device {:?}: key is not hex", d.name))?;
            if key.len() != 32 {
                bail!("device {:?}: key must be 32 bytes (64 hex digits), got {}", d.name, key.len());
            }
        }
        if !matches!(self.server.log_format.as_str(), "json" | "text") {
            bail!("server.log_format must be \"json\" or \"text\"");
        }
        Ok(())
    }

    /// Devices with decoded keys.
    pub fn devices(&self) -> Vec<Device> {
        self.devices
            .iter()
            .map(|d| Device {
                name: d.name.clone(),
                device_id: d.device_id.0,
                key: crate::board::unhex(&d.key).expect("validated"),
            })
            .collect()
    }

    /// SQLite path.
    pub fn db_path(&self) -> PathBuf {
        self.server.data_dir.join("sapflux.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let text = include_str!("../../config.example.toml");
        let cfg = Config::parse(text).unwrap();
        assert_eq!(cfg.devices.len(), 1);
        assert!(sapflux_uplink::validate_schedule(&cfg.defaults.schedule()).is_ok());
    }

    #[test]
    fn rejects_bad_key_and_hex_id() {
        let base = "[server]\ndata_dir = \"/tmp/x\"\n[[device]]\nname = \"a\"\n";
        assert!(Config::parse(&format!("{base}device_id = 1\nkey = \"00\"\n")).is_err());
        let ok = format!("{base}device_id = \"0x10\"\nkey = \"{}\"\n", "ab".repeat(32));
        assert_eq!(Config::parse(&ok).unwrap().devices[0].device_id, DeviceId(16));
    }
}
