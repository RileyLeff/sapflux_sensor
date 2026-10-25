//! `sapflux`: admin CLI. Works on the server's SQLite database directly
//! (WAL lets it run next to the live server; changes reach the gateway at
//! its next check-in).
//!
//! The remote off switch:   sapflux off box1
//! Turn it back on:         sapflux on box1

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use sapflux_server::config::{Config, ProbeType};
use sapflux_server::store::{DeviceView, SettingsPatch, Store};
use sapflux_server::{board, fmt_utc, now_ms, window};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "sap-flux server admin. `sapflux off <device>` stops pulsing at the next check-in.")]
struct Cli {
    /// Server config file (for data_dir and device names).
    #[arg(long, short, global = true, env = "SAPFLUX_CONFIG", default_value = "/etc/sapflux/server.toml")]
    config: PathBuf,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Turn pulsing OFF for a device (the remote off switch). Takes effect at the next check-in.
    #[command(visible_alias = "disable")]
    Off { device: String },
    /// Turn pulsing ON for a device. Takes effect at the next check-in.
    #[command(visible_alias = "enable")]
    On { device: String },
    /// Show devices: last check-in, gateway, decoded board FieldStatus, schedule sync.
    #[command(visible_alias = "devices")]
    Status { device: Option<String> },
    /// Show or change the schedule and gateway settings (validated like the board does).
    Schedule {
        device: String,
        #[arg(long)]
        pulse_ms: Option<u16>,
        #[arg(long)]
        interval_s: Option<u32>,
        #[arg(long)]
        phase_s: Option<u32>,
        #[arg(long)]
        baseline_s: Option<u16>,
        #[arg(long)]
        recovery_s: Option<u16>,
        #[arg(long)]
        sample_hz: Option<u16>,
        #[arg(long)]
        bin_ms: Option<u16>,
        /// Send windows as SFK1 records (about 0.25 kB instead of 12.6 kB).
        #[arg(long)]
        sfk1: Option<bool>,
        /// Probe on port 1 (picks the SFK1 profile).
        #[arg(long, value_enum)]
        probe1: Option<ProbeType>,
        #[arg(long, value_enum)]
        probe2: Option<ProbeType>,
        /// Probe 1 installed upside down (recorded with each window).
        #[arg(long)]
        probe1_flipped: Option<bool>,
        #[arg(long)]
        probe2_flipped: Option<bool>,
        #[arg(long)]
        require_lease: Option<bool>,
        #[arg(long)]
        lease_s: Option<u32>,
        #[arg(long)]
        wake_interval_s: Option<u32>,
        #[arg(long)]
        send_windows: Option<bool>,
    },
    /// Bump config_version without changing anything.
    Bump { device: String },
    /// Ask the gateway to reboot (sent until it checks in with a new boot_id).
    RebootGateway { device: String },
    /// List stored windows (newest first).
    Windows {
        #[arg(long)]
        board: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: u32,
        /// Also list windows still being reassembled.
        #[arg(long)]
        partial: bool,
    },
    /// Write a window's decoded CSV (to stdout or -o FILE).
    Export {
        board: String,
        run_id: u32,
        #[arg(long)]
        generation: Option<u32>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show events (gateway and server), newest first.
    Events {
        device: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: u32,
    },
    /// Settings change history.
    History {
        device: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Generate a 32-byte device key and print the config snippets.
    Keygen {
        #[arg(long, default_value = "box1")]
        name: String,
        /// Gateway device_id (u64 from the IMEI).
        #[arg(long, default_value_t = 0)]
        device_id: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Cmd::Keygen { name, device_id } = &cli.cmd {
        return keygen(name, *device_id);
    }
    let cfg = Config::load(&cli.config)?;
    let store = Store::open(&cfg.server.data_dir)?;
    store.sync_devices(&cfg)?;
    let actor = format!("cli:{}", std::env::var("USER").unwrap_or_else(|_| "?".into()));
    let out = |v: &dyn erased::Json| -> Result<()> {
        println!("{}", v.to_json()?);
        Ok(())
    };
    match cli.cmd {
        Cmd::Off { device } => {
            let id = store.resolve(&device)?;
            let s = store.set_enabled(id, false, &actor)?;
            if cli.json {
                return out(&s);
            }
            println!(
                "{}: pulsing OFF (config v{}). The gateway pushes this to the board at its next check-in (every {} s).",
                s.name, s.config_version, s.wake_interval_s
            );
        }
        Cmd::On { device } => {
            let id = store.resolve(&device)?;
            let s = store.set_enabled(id, true, &actor)?;
            if cli.json {
                return out(&s);
            }
            println!(
                "{}: pulsing ON (config v{}): {}. Reaches the board at the next check-in (every {} s).",
                s.name,
                s.config_version,
                describe_schedule(&s),
                s.wake_interval_s
            );
        }
        Cmd::Status { device } => {
            let devs = match device {
                Some(d) => vec![store.device(store.resolve(&d)?)?],
                None => store.devices()?,
            };
            if cli.json {
                return out(&devs);
            }
            if devs.is_empty() {
                println!("no devices; add [[device]] entries to {}", cli.config.display());
            }
            for d in &devs {
                print_device(d);
            }
        }
        Cmd::Schedule {
            device,
            pulse_ms,
            interval_s,
            phase_s,
            baseline_s,
            recovery_s,
            sample_hz,
            bin_ms,
            sfk1,
            probe1,
            probe2,
            probe1_flipped,
            probe2_flipped,
            require_lease,
            lease_s,
            wake_interval_s,
            send_windows,
        } => {
            let id = store.resolve(&device)?;
            let patch = SettingsPatch {
                enabled: None,
                require_lease,
                pulse_duration_ms: pulse_ms,
                interval_s,
                phase_s,
                baseline_s,
                recovery_s,
                sample_hz,
                bin_ms,
                sfk1,
                probe1,
                probe2,
                probe1_flipped,
                probe2_flipped,
                lease_s,
                wake_interval_s,
                send_windows,
            };
            let before = store.settings(id)?;
            let s = store.update_settings(id, &patch, &actor)?;
            if cli.json {
                return out(&s);
            }
            if s.config_version != before.config_version {
                println!("updated (config v{} -> v{})", before.config_version, s.config_version);
            }
            println!("{}: {}", s.name, describe_schedule(&s));
            println!(
                "  gateway: wake every {} s, lease {} s, send windows {}, schedule_crc 0x{:04x}",
                s.wake_interval_s, s.lease_s, s.send_windows, s.schedule_crc
            );
        }
        Cmd::Bump { device } => {
            let s = store.bump_version(store.resolve(&device)?, &actor)?;
            if cli.json {
                return out(&s);
            }
            println!("{}: config v{}", s.name, s.config_version);
        }
        Cmd::RebootGateway { device } => {
            let id = store.resolve(&device)?;
            store.request_reboot(id, &actor)?;
            println!("reboot requested; sent with every check-in until the gateway comes back with a new boot_id");
        }
        Cmd::Windows { board, limit, partial } => {
            let rows = store.windows(board.as_deref(), limit)?;
            if cli.json {
                return out(&rows);
            }
            println!(
                "{:<16} {:>10} {:>3} {:<4} {:<24} {:>5} {:>6} {:>5}  flags",
                "board", "run_id", "gen", "ok", "heater on (UTC)", "bins", "frames", "miss"
            );
            for w in &rows {
                println!(
                    "{:<16} {:>10} {:>3} {:<4} {:<24} {:>5} {:>6} {:>5}  {}{}",
                    w.board_id,
                    w.run_id,
                    w.generation,
                    w.status,
                    w.heater_on_unix_ms.map(|t| fmt_utc(t as i64)).unwrap_or_else(|| "-".into()),
                    w.n_bins.map(|n| n.to_string()).unwrap_or_default(),
                    w.frames_total.map(|n| n.to_string()).unwrap_or_default(),
                    w.frames_missing.map(|n| n.to_string()).unwrap_or_default(),
                    w.flags.join(","),
                    w.error.as_ref().map(|e| format!(" ERROR: {e}")).unwrap_or_default()
                );
            }
            if partial {
                for p in store.partials()? {
                    println!("partial: {p}");
                }
            }
        }
        Cmd::Export {
            board,
            run_id,
            generation,
            output,
        } => {
            let w = store
                .window(&board, run_id, generation)?
                .with_context(|| format!("no window {board}/{run_id}"))?;
            let csv_path = PathBuf::from(format!("{}.csv", w.stem));
            let csv = match std::fs::read(&csv_path) {
                Ok(b) => b,
                Err(_) => {
                    // Regenerate from the raw bytes.
                    let raw = std::fs::read(format!("{}.bin", w.stem))?;
                    let (info, data) = window::verify(&raw, Some(run_id))?;
                    let mut v = Vec::new();
                    window::write_csv(&window::decode(&info, data), &mut v)?;
                    v
                }
            };
            match output {
                Some(p) => {
                    std::fs::write(&p, &csv)?;
                    eprintln!("wrote {}", p.display());
                }
                None => std::io::stdout().write_all(&csv)?,
            }
        }
        Cmd::Events { device, limit } => {
            let id = device.map(|d| store.resolve(&d)).transpose()?;
            let ev = store.events(id, limit)?;
            if cli.json {
                return out(&ev);
            }
            for e in &ev {
                println!(
                    "{}  {:<7} dev {:<16} code {:<4} {}",
                    fmt_utc(e.t_unix_ms),
                    e.source,
                    e.device_id.map(|d| d.to_string()).unwrap_or_default(),
                    e.code,
                    e.text
                );
            }
        }
        Cmd::History { device, limit } => {
            let h = store.config_log(store.resolve(&device)?, limit)?;
            if cli.json {
                return out(&h);
            }
            for e in &h {
                let s = &e["settings"];
                println!(
                    "{}  v{:<4} {:<20} enabled={} pulse={} interval={} phase={} baseline={} recovery={} hz={} bin={} lease={} wake={}",
                    fmt_utc(e["t_ms"].as_i64().unwrap_or(0)),
                    e["config_version"],
                    e["actor"].as_str().unwrap_or(""),
                    s["enabled"], s["pulse_duration_ms"], s["interval_s"], s["phase_s"],
                    s["baseline_s"], s["recovery_s"], s["sample_hz"], s["bin_ms"], s["lease_s"],
                    s["wake_interval_s"]
                );
            }
        }
        Cmd::Keygen { .. } => unreachable!(),
    }
    Ok(())
}

mod erased {
    pub trait Json {
        fn to_json(&self) -> anyhow::Result<String>;
    }
    impl<T: serde::Serialize> Json for T {
        fn to_json(&self) -> anyhow::Result<String> {
            Ok(serde_json::to_string_pretty(self)?)
        }
    }
}

fn describe_schedule(s: &sapflux_server::store::Settings) -> String {
    let probe = |p: ProbeType, flipped: bool| {
        let name = match p {
            ProbeType::None => "none",
            ProbeType::East30 => "east30",
            ProbeType::East30Offset => "east30_offset",
        };
        if flipped { format!("{name} (flipped)") } else { name.to_string() }
    };
    format!(
        "{} - pulse {} ms every {} s (phase {} s), baseline {} s, recovery {} s, {} Hz, {}; probes {}, {}{}",
        if s.enabled { "ON" } else { "OFF" },
        s.pulse_duration_ms,
        s.interval_s,
        s.phase_s,
        s.baseline_s,
        s.recovery_s,
        s.sample_hz,
        if s.sfk1 { "SFK1".to_string() } else { format!("{} ms bins", s.bin_ms) },
        probe(s.probe1, s.probe1_flipped),
        probe(s.probe2, s.probe2_flipped),
        if s.require_lease { ", REQUIRE_LEASE" } else { "" }
    )
}

fn ago(ms: i64) -> String {
    let s = (now_ms() - ms) / 1000;
    match s {
        s if s < 120 => format!("{s} s ago"),
        s if s < 7200 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{:.1} h ago", s as f64 / 3600.0),
        s => format!("{:.1} days ago", s as f64 / 86_400.0),
    }
}

fn print_device(d: &DeviceView) {
    let s = &d.settings;
    println!("{}  (device_id {})", d.name, d.device_id);
    println!("  schedule:   {}", describe_schedule(s));
    println!(
        "              config v{}, schedule_crc 0x{:04x}, set {} by {}",
        s.config_version,
        s.schedule_crc,
        fmt_utc(s.updated_ms),
        s.updated_by
    );
    match (d.schedule_in_sync, &d.field_status) {
        (Some(true), _) => println!("  board sync: board has this schedule"),
        (Some(false), Some(f)) => println!(
            "  board sync: NOT YET - board reports schedule_crc 0x{:04x}; gateway pushes it at the next check-in",
            f.schedule_crc
        ),
        _ => println!("  board sync: unknown (no FieldStatus received yet)"),
    }
    match d.last_seen_ms {
        None => println!("  check-in:   never"),
        Some(t) => {
            println!(
                "  check-in:   {} ({}) from {} boot_id 0x{:08x}",
                fmt_utc(t),
                ago(t),
                d.last_peer.as_deref().unwrap_or("?"),
                d.last_boot_id.unwrap_or(0)
            );
            println!(
                "  gateway:    fw 0x{:08x}, batt {} mV, RSRP {} dBm, uptime {} s, flags 0x{:x}, t2-t1 {} ms",
                d.gw_fw.unwrap_or(0),
                d.batt_mv.unwrap_or(0),
                d.rsrp_dbm.unwrap_or(0),
                d.uptime_s.unwrap_or(0),
                d.checkin_flags.unwrap_or(0),
                d.last_t2_minus_t1_ms.unwrap_or(0)
            );
        }
    }
    println!(
        "  settings:   wake every {} s, lease {} s, send windows {}, reboot pending {}",
        s.wake_interval_s, s.lease_s, s.send_windows, s.reboot_pending
    );
    if let Some(b) = &d.board_id {
        println!(
            "  board:      {} (frames {})",
            b,
            d.board_seen_ms.map(ago).unwrap_or_default()
        );
    }
    if let Some(frames) = d.board.as_array() {
        for f in frames {
            match f["type"].as_str() {
                Some("hello") => println!("    Hello:       fw {} caps 0x{:x}", f["firmware"].as_str().unwrap_or("?"), f["capabilities"].as_u64().unwrap_or(0)),
                Some("status") => println!(
                    "    Status:      {} run {} fault {} pulse {} ms cooldown {} ms rtc {}",
                    f["state"].as_str().unwrap_or("?"),
                    f["run_id"],
                    f["fault_code"],
                    f["pulse_duration_ms"],
                    f["cooldown_remaining_ms"],
                    f["rtc_unix_ms"].as_i64().filter(|&t| t >= 0).map(fmt_utc).unwrap_or_else(|| "invalid".into())
                ),
                Some("field_status") => {}
                Some(t) => println!("    {t}: {f}"),
                None => println!("    {f}"),
            }
        }
    }
    if let Some(f) = &d.field_status {
        println!("    FieldStatus: flags [{}]", f.flag_names().join(", "));
        println!(
            "                 next pulse {}, windows pending {} (oldest run {}), dropped {}, last run {}",
            if f.next_pulse_unix_s == 0 { "none".into() } else { fmt_utc(f.next_pulse_unix_s as i64 * 1000) },
            f.windows_pending,
            f.oldest_pending_run_id,
            f.windows_dropped,
            f.last_run_id
        );
        println!(
            "                 RTC {} trim {} ppb, last SetRtc step {} ms, boots {}, v24_raw {}, flash {} KiB",
            if f.rtc_unix_ms >= 0 { fmt_utc(f.rtc_unix_ms) } else { "invalid".into() },
            f.calr_ppb,
            f.last_rtc_step_ms,
            f.boot_count,
            f.v24_raw,
            f.flash_used_kib
        );
    }
    println!();
}

fn keygen(name: &str, device_id: u64) -> Result<()> {
    let mut key = [0u8; 32];
    let mut f = std::fs::File::open("/dev/urandom").context("opening /dev/urandom")?;
    std::io::Read::read_exact(&mut f, &mut key)?;
    if key.iter().all(|&b| b == 0) {
        bail!("RNG returned zeros");
    }
    let hex = board::hex(&key);
    println!("# --- server: add to /etc/sapflux/server.toml ---");
    println!("[[device]]");
    println!("name = \"{name}\"");
    println!("device_id = {device_id}");
    println!("key = \"{hex}\"");
    println!();
    println!("# --- gateway: gitignored compile-time config ---");
    println!("SAPFLUX_DEVICE_ID={device_id}");
    println!("SAPFLUX_KEY_HEX={hex}");
    let bytes: Vec<String> = key.iter().map(|b| format!("0x{b:02x}")).collect();
    println!("// Rust: pub const DEVICE_KEY: [u8; 32] = [{}];", bytes.join(", "));
    Ok(())
}
