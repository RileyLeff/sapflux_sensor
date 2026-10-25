//! `sap-flux-field`: bench CLI for the board's field mode.

use clap::{Args, Parser, Subcommand};
use sap_flux_field_cli::link::{host_unix_ms, Link, LinkError, LinkOptions, QueryReply};
use sap_flux_field_cli::raw;
use sap_flux_field_cli::serial::{candidate_ports, SerialTransport, DEFAULT_BAUD};
use sap_flux_field_cli::window::{self, Window};
use sap_flux_protocol::field::ScheduleError;
use sap_flux_protocol::{
    Ack, FieldStatus, Schedule, SetSchedule, FIELD_STATUS_EEPROM_FAULT, FIELD_STATUS_ENABLED,
    FIELD_STATUS_FLASH_OK,
    FIELD_STATUS_HEATER_LOCKOUT, FIELD_STATUS_LEASE_VALID, FIELD_STATUS_REQUIRE_LEASE,
    FIELD_STATUS_RTC_VALID, SCHEDULE_FLAG_REQUIRE_LEASE, SCHEDULE_FLAG_SFK1, ProbeKind, Sfk1Preset,
};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "sap-flux-field",
    version,
    about = "Bench-test the sap-flux board's field mode without the gateway",
    long_about = "Talks the field protocol (docs/field-protocol.md section 1) to the board over \
                  a USB-serial adapter on J6 (115200 8N1) or the RS-485 adapter (--rs485). \
                  Every command is preceded by a single 0x00 wake byte and a >= 10 ms pause, \
                  and re-sent up to --retries times when no reply arrives."
)]
struct Cli {
    /// Serial port (e.g. /dev/cu.usbserial-XXXX). Defaults to $SAP_FLUX_PORT,
    /// else the only candidate port present.
    #[arg(short, long, global = true, env = "SAP_FLUX_PORT")]
    port: Option<String>,
    /// Baud rate.
    #[arg(long, global = true, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    /// Drive RTS as the RS-485 driver enable (FT232R bench adapter on J5).
    #[arg(long, global = true)]
    rs485: bool,
    /// Reply timeout per attempt, ms (the board may hold replies ≤ 1.5 s
    /// while the heater is on).
    #[arg(long, global = true, default_value_t = 1800)]
    timeout_ms: u64,
    /// Re-sends after a timed-out attempt.
    #[arg(long, global = true, default_value_t = 3)]
    retries: u32,
    /// Wake-byte to command delay, ms (spec minimum 10).
    #[arg(long, global = true, default_value_t = 15)]
    wake_ms: u64,
    /// Print every frame sent and received.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List serial ports.
    Ports,
    /// Query: Hello + Status (+ FieldStatus in field mode).
    Query,
    /// GetFieldStatus.
    Status,
    /// SetSchedule (defaults: docs/field-protocol.md section 3).
    SetSchedule(ScheduleArgs),
    /// SetRtc from the host clock; reports the board offset before and after.
    SetRtc(RtcArgs),
    /// GetWindow: download a window, verify data_crc32, write CSV + uplink bytes.
    GetWindow(GetWindowArgs),
    /// GetRaw: download a run's raw 25 Hz frame log, verify every block,
    /// write raw_<run>.csv + raw_<run>.bin.
    GetRaw(GetRawArgs),
    /// AckWindow: mark a window delivered (the board frees it).
    AckWindow {
        /// Run id to acknowledge.
        #[arg(long)]
        run: u32,
    },
}

#[derive(Args)]
struct ScheduleArgs {
    /// Enable pulsing on this schedule (one of --enable/--disable is required).
    #[arg(long, required_unless_present = "disable", conflicts_with = "disable")]
    enable: bool,
    /// Store the schedule disabled.
    #[arg(long)]
    disable: bool,
    /// Heater pulse, ms (500, 1000, 1500).
    #[arg(long, default_value_t = 1000)]
    pulse_ms: u16,
    /// Pulse period, s (≥ 600).
    #[arg(long, default_value_t = 3600)]
    interval_s: u32,
    /// Phase: pulses at t with (t - phase) mod interval == 0, Unix s.
    #[arg(long, default_value_t = 0)]
    phase_s: u32,
    /// Baseline before the pulse, s (10–300).
    #[arg(long, default_value_t = 60)]
    baseline_s: u16,
    /// Recovery after the pulse, s (60–600).
    #[arg(long, default_value_t = 180)]
    recovery_s: u16,
    /// Sample rate, Hz (10, 20, 25).
    #[arg(long, default_value_t = 25)]
    sample_hz: u16,
    /// Uplink bin width, ms (250, 500, 1000).
    #[arg(long, default_value_t = 500)]
    bin_ms: u16,
    /// Record windows as SFK1 records (one per probe with a kind).
    #[arg(long)]
    sfk1: bool,
    /// Probe on port 1; picks its SFK1 profile.
    #[arg(long, value_enum, default_value_t = Probe::None)]
    probe1: Probe,
    /// Probe on port 2.
    #[arg(long, value_enum, default_value_t = Probe::None)]
    probe2: Probe,
    /// SFK1 preset for port 1: knots and prior, as 12, 20-mixed, 24-fast…
    /// The board refuses a preset its firmware does not link.
    #[arg(long, value_parser = str::parse::<Sfk1Preset>, default_value = "12")]
    preset1: Sfk1Preset,
    /// SFK1 preset for port 2.
    #[arg(long, value_parser = str::parse::<Sfk1Preset>, default_value = "12")]
    preset2: Sfk1Preset,
    /// Set REQUIRE_LEASE: only pulse while now < lease_until.
    #[arg(long)]
    require_lease: bool,
    /// Lease length from now, s (sets lease_until_unix_s).
    #[arg(long, conflicts_with = "lease_until")]
    lease_s: Option<u32>,
    /// Absolute lease end, Unix s.
    #[arg(long)]
    lease_until: Option<u32>,
    /// Board cooldown used for the local limit check, ms.
    #[arg(long, default_value_t = 600_000)]
    cooldown_ms: u32,
    /// Send even if the local limit check fails (to test board rejection).
    #[arg(long)]
    no_validate: bool,
}

/// Probe kind for `--probe1` / `--probe2`.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Probe {
    None,
    /// East30, both needles 6 mm from the heater.
    East30,
    /// East30 offset probe, needles 10 mm and 5 mm from the heater.
    East30Offset,
}

impl Probe {
    fn kind(self) -> ProbeKind {
        match self {
            Probe::None => ProbeKind::None,
            Probe::East30 => ProbeKind::East30,
            Probe::East30Offset => ProbeKind::East30Offset,
        }
    }
}

#[derive(Args)]
struct RtcArgs {
    /// Only set the RTC when |offset| exceeds this, ms (the gateway uses 250).
    #[arg(long, default_value_t = 0)]
    min_offset_ms: i64,
    /// Report the offset without setting.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct GetWindowArgs {
    /// Run id; 0 = oldest undelivered.
    #[arg(long, default_value_t = 0)]
    run: u32,
    /// Output directory for window_<run>.csv and window_<run>.bin.
    #[arg(long, default_value = ".")]
    out_dir: PathBuf,
    /// Send AckWindow after a verified download.
    #[arg(long)]
    ack: bool,
    /// Only print the header (no chunk download).
    #[arg(long)]
    header_only: bool,
}

#[derive(Args)]
struct GetRawArgs {
    /// Run id (as in WindowInfo / FieldStatus last_run_id).
    #[arg(long)]
    run: u32,
    /// Output directory for raw_<run>.csv and raw_<run>.bin.
    #[arg(long, default_value = ".")]
    out_dir: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type BoxError = Box<dyn std::error::Error>;

fn run(cli: Cli) -> Result<(), BoxError> {
    if let Command::Ports = cli.command {
        for p in candidate_ports() {
            println!("{p}");
        }
        return Ok(());
    }
    let port = match cli.port.clone() {
        Some(p) => p,
        None => {
            let ports: Vec<String> = candidate_ports()
                .into_iter()
                .filter(|p| p.contains("usbserial") || p.contains("usbmodem"))
                .collect();
            match ports.as_slice() {
                [one] => one.clone(),
                [] => return Err("no USB serial port found; pass --port".into()),
                many => {
                    return Err(format!(
                        "several serial ports found ({}); pass --port",
                        many.join(", ")
                    )
                    .into())
                }
            }
        }
    };
    if let Command::SetSchedule(a) = &cli.command {
        if !a.no_validate {
            build_schedule(a)?; // fail fast on bad arguments
        }
    }
    if cli.wake_ms < 10 {
        return Err("--wake-ms must be at least 10 (docs/field-protocol.md)".into());
    }
    let transport = SerialTransport::open(&port, cli.baud, cli.rs485)
        .map_err(|e| format!("open {port}: {e}"))?;
    let options = LinkOptions {
        wake_delay: Duration::from_millis(cli.wake_ms),
        timeout: Duration::from_millis(cli.timeout_ms),
        retries: cli.retries,
        verbose: cli.verbose,
        ..LinkOptions::default()
    };
    let mut link = Link::new(transport, options);
    if cli.verbose {
        eprintln!(
            "port {port} @ {} baud{}",
            cli.baud,
            if cli.rs485 { " (RS-485)" } else { "" }
        );
    }

    match cli.command {
        Command::Ports => unreachable!(),
        Command::Query => {
            let q = link.query()?;
            print_query(&q);
        }
        Command::Status => {
            let (fs, _, _) = link.field_status()?;
            print_field_status(&fs);
        }
        Command::SetSchedule(a) => set_schedule(&mut link, a)?,
        Command::SetRtc(a) => set_rtc(&mut link, a, cli.baud)?,
        Command::GetWindow(a) => get_window(&mut link, a)?,
        Command::GetRaw(a) => get_raw(&mut link, a)?,
        Command::AckWindow { run } => {
            let r = link.ack_window(run)?;
            print_ack("AckWindow", &r.ack);
            if r.ack.result != 0 {
                return Err(
                    format!("AckWindow rejected ({})", ack_result_name(r.ack.result)).into(),
                );
            }
        }
    }
    if link.retries_used > 0 {
        eprintln!(
            "({} retr{} used)",
            link.retries_used,
            if link.retries_used == 1 { "y" } else { "ies" }
        );
    }
    Ok(())
}

fn ack_result_name(r: u8) -> String {
    match r {
        0x00 => "OK".into(),
        0x02 => "INVALID_PARAMETER".into(),
        0x03 => "BACKUP_WRITE_FAILED".into(),
        0x04 => "NOT_FOUND".into(),
        0x10 => "WRONG_STATE".into(),
        0x11 => "WRONG_RUN".into(),
        other => format!("{other:#04x}"),
    }
}

fn print_ack(name: &str, a: &Ack) {
    println!(
        "{name}: ack result {} ({}) run {}",
        a.result,
        ack_result_name(a.result),
        a.run_id
    );
}

fn field_flag_names(flags: u8) -> String {
    let names = [
        (FIELD_STATUS_ENABLED, "enabled"),
        (FIELD_STATUS_REQUIRE_LEASE, "require_lease"),
        (FIELD_STATUS_LEASE_VALID, "lease_valid"),
        (FIELD_STATUS_RTC_VALID, "rtc_valid"),
        (FIELD_STATUS_HEATER_LOCKOUT, "heater_lockout"),
        (FIELD_STATUS_FLASH_OK, "flash_ok"),
        (FIELD_STATUS_EEPROM_FAULT, "eeprom_fault"),
    ];
    let set: Vec<&str> = names
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if set.is_empty() {
        "-".into()
    } else {
        set.join("|")
    }
}

fn fmt_unix_ms(ms: i64) -> String {
    if ms < 0 {
        return "invalid".into();
    }
    format!("{ms} ms")
}

fn print_field_status(fs: &FieldStatus) {
    println!("FieldStatus");
    println!("  monotonic_us          {}", fs.monotonic_us);
    println!("  rtc_unix_ms           {}", fmt_unix_ms(fs.rtc_unix_ms));
    println!(
        "  next_pulse_unix_s     {}",
        if fs.next_pulse_unix_s == 0 {
            "0 (disabled)".to_string()
        } else {
            let dt = fs.next_pulse_unix_s as i64 - host_unix_ms() / 1000;
            format!("{} (in {dt} s by host clock)", fs.next_pulse_unix_s)
        }
    );
    println!(
        "  schedule_flags        {:#04x} {}",
        fs.schedule_flags,
        field_flag_names(fs.schedule_flags)
    );
    println!("  windows_pending       {}", fs.windows_pending);
    println!("  windows_dropped       {}", fs.windows_dropped);
    println!("  oldest_pending_run_id {}", fs.oldest_pending_run_id);
    println!("  last_run_id           {}", fs.last_run_id);
    println!("  calr_ppb              {}", fs.calr_ppb);
    println!("  last_rtc_step_ms      {}", fs.last_rtc_step_ms);
    println!("  boot_count            {}", fs.boot_count);
    println!("  v24_raw               {}", fs.v24_raw);
    println!("  flash_used_kib        {}", fs.flash_used_kib);
    println!("  schedule_crc          {:04x}", fs.schedule_crc);
}

fn print_query(q: &QueryReply) {
    let h = &q.hello;
    println!(
        "Hello: board_id {:016X} fw {}.{}.{} caps {:#x}",
        h.board_id,
        (h.firmware_version >> 16) & 0xFFFF,
        (h.firmware_version >> 8) & 0xFF,
        h.firmware_version & 0xFF,
        h.capabilities
    );
    let s = &q.status;
    println!(
        "Status: state {:?} run {} fault {} pulse_ms {} cooldown_ms {} rtc {}",
        s.state,
        s.run_id,
        s.fault_code,
        s.pulse_duration_ms,
        s.cooldown_remaining_ms,
        fmt_unix_ms(s.rtc_unix_ms)
    );
    if let Some(off) = offset_ms(s.rtc_unix_ms, q.sent_unix_ms, q.status_rx_unix_ms) {
        println!("  board - host offset ≈ {off} ms");
    }
    match &q.field_status {
        Some(fs) => print_field_status(fs),
        None => println!("(no FieldStatus: board not in field mode)"),
    }
}

/// Board − host clock offset, taking the host time at the midpoint of the
/// exchange. `None` when the board RTC is invalid.
fn offset_ms(board_ms: i64, sent_ms: i64, rx_ms: i64) -> Option<i64> {
    if board_ms < 0 {
        return None;
    }
    Some(board_ms - (sent_ms + rx_ms) / 2)
}

fn schedule_error_text(e: ScheduleError) -> &'static str {
    match e {
        ScheduleError::Enabled => "enabled must be 0 or 1",
        ScheduleError::Flags => "unknown schedule flag bits",
        ScheduleError::PulseDuration => "pulse must be 500, 1000 or 1500 ms",
        ScheduleError::Interval => "interval must be >= 600 s",
        ScheduleError::Baseline => "baseline must be 10-300 s",
        ScheduleError::Recovery => "recovery must be 60-600 s",
        ScheduleError::Timing => "baseline + recovery + cooldown must be < interval",
        ScheduleError::SampleHz => "sample rate must be 10, 20 or 25 Hz",
        ScheduleError::BinMs => "bin must be 250, 500 or 1000 ms",
        ScheduleError::Sfk1 => "SFK1 needs 25 Hz and a probe kind (--probe1/--probe2)",
        ScheduleError::WindowTooLong => "baseline + pulse + recovery + 4 s must fit in 1000 bins",
        ScheduleError::Sfk1Preset => "unknown SFK1 preset code",
    }
}

/// Build and locally validate the schedule (before any port is opened).
fn build_schedule(a: &ScheduleArgs) -> Result<Schedule, BoxError> {
    let schedule = Schedule {
        enabled: a.enable as u8,
        flags: if a.require_lease {
            SCHEDULE_FLAG_REQUIRE_LEASE
        } else {
            0
        } | if a.sfk1 { SCHEDULE_FLAG_SFK1 } else { 0 },
        pulse_duration_ms: a.pulse_ms,
        interval_s: a.interval_s,
        phase_s: a.phase_s,
        baseline_s: a.baseline_s,
        recovery_s: a.recovery_s,
        sample_hz: a.sample_hz,
        bin_ms: a.bin_ms,
        sfk1_presets: [0; 2],
    }
    .with_probe(0, a.probe1.kind())
    .with_probe(1, a.probe2.kind())
    .with_preset(0, a.preset1)
    .with_preset(1, a.preset2);
    let _ = a.disable; // clap guarantees exactly one of --enable/--disable
    if let Err(e) = schedule.validate(a.cooldown_ms) {
        if a.no_validate {
            eprintln!("warning: {} (sending anyway)", schedule_error_text(e));
        } else {
            return Err(format!(
                "{} (use --no-validate to send anyway)",
                schedule_error_text(e)
            )
            .into());
        }
    }
    Ok(schedule)
}

fn set_schedule<T: sap_flux_field_cli::link::Transport>(
    link: &mut Link<T>,
    a: ScheduleArgs,
) -> Result<(), BoxError> {
    let schedule = build_schedule(&a)?;
    let lease_until_unix_s = match (a.lease_s, a.lease_until) {
        (Some(s), _) => (host_unix_ms() / 1000) as u32 + s,
        (None, Some(u)) => u,
        (None, None) => 0,
    };
    if a.require_lease && lease_until_unix_s == 0 {
        eprintln!("warning: REQUIRE_LEASE with lease_until 0: the board will not pulse");
    }
    println!(
        "SetSchedule {:?} lease_until {} (window {} bins), local schedule_crc {:04x}",
        schedule,
        lease_until_unix_s,
        schedule.window_bins(),
        schedule.crc16()
    );
    let r = link.set_schedule(SetSchedule {
        command_nonce: 0,
        schedule,
        lease_until_unix_s,
    })?;
    print_ack("SetSchedule", &r.ack);
    if let Some(fs) = &r.field_status {
        print_field_status(fs);
        if r.ack.result == 0 && fs.schedule_crc != schedule.crc16() {
            return Err(format!(
                "board schedule_crc {:04x} != local {:04x}",
                fs.schedule_crc,
                schedule.crc16()
            )
            .into());
        }
    } else {
        eprintln!("warning: no FieldStatus after Ack");
    }
    if r.ack.result != 0 {
        return Err(format!("SetSchedule rejected ({})", ack_result_name(r.ack.result)).into());
    }
    Ok(())
}

fn set_rtc<T: sap_flux_field_cli::link::Transport>(
    link: &mut Link<T>,
    a: RtcArgs,
    baud: u32,
) -> Result<(), BoxError> {
    let before = link.query()?;
    let off = offset_ms(
        before.status.rtc_unix_ms,
        before.sent_unix_ms,
        before.status_rx_unix_ms,
    );
    match off {
        Some(o) => println!("before: board - host offset ≈ {o} ms"),
        None => println!("before: board RTC invalid"),
    }
    if a.dry_run {
        return Ok(());
    }
    if let Some(o) = off {
        if o.abs() <= a.min_offset_ms {
            println!("|offset| <= {} ms: not setting", a.min_offset_ms);
            return Ok(());
        }
    }
    // Stamp SetRtc for when its last wire byte arrives (28-byte frame +
    // COBS code byte + delimiter). The board then steps on the next whole
    // second, so its Ack can take up to 1 s longer than usual.
    let frame_bytes = (sap_flux_protocol::MIN_FRAME_LEN + 16 + 2) as u64;
    let lead_ms = (frame_bytes * 10 * 1000).div_ceil(baud as u64) as i64;
    let saved = link.options.timeout;
    link.options.timeout = saved + Duration::from_millis(1000);
    let result = link.set_rtc_now(lead_ms);
    link.options.timeout = saved;
    let (r, sent) = result?;
    print_ack("SetRtc", &r.ack);
    println!("sent unix_ms {sent}");
    if r.ack.result != 0 {
        return Err(format!("SetRtc rejected ({})", ack_result_name(r.ack.result)).into());
    }
    let after = link.query()?;
    match offset_ms(
        after.status.rtc_unix_ms,
        after.sent_unix_ms,
        after.status_rx_unix_ms,
    ) {
        Some(o) => println!("after:  board - host offset ≈ {o} ms"),
        None => println!("after:  board RTC still invalid"),
    }
    if let Some(fs) = after.field_status {
        println!(
            "board last_rtc_step_ms {} calr_ppb {}",
            fs.last_rtc_step_ms, fs.calr_ppb
        );
    }
    Ok(())
}

fn get_window<T: sap_flux_field_cli::link::Transport>(
    link: &mut Link<T>,
    a: GetWindowArgs,
) -> Result<(), BoxError> {
    if a.header_only {
        let info = link.window_info(a.run).map_err(not_found_hint)?;
        let w = Window {
            info,
            data: Vec::new(),
        };
        println!("{}", w.summary());
        return Ok(());
    }
    let w = window::download(link, a.run, |done, total| {
        if done == total || done % 20 == 0 {
            eprint!("\rchunk {done}/{total}");
        }
        if done == total {
            eprintln!();
        }
    })
    .map_err(|e| match e {
        window::WindowError::Link(l) => not_found_hint(l),
        other => other.into(),
    })?;
    println!("{}", w.summary());
    println!("data_crc32 verified");
    std::fs::create_dir_all(&a.out_dir)?;
    let csv = a.out_dir.join(format!("window_{}.csv", w.info.run_id));
    let bin = a.out_dir.join(format!("window_{}.bin", w.info.run_id));
    w.write_csv(std::io::BufWriter::new(std::fs::File::create(&csv)?))?;
    std::fs::write(&bin, w.uplink_bytes())?;
    println!("wrote {} and {}", csv.display(), bin.display());
    if a.ack {
        let r = link.ack_window(w.info.run_id)?;
        print_ack("AckWindow", &r.ack);
        if r.ack.result != 0 {
            return Err(format!("AckWindow rejected ({})", ack_result_name(r.ack.result)).into());
        }
    }
    Ok(())
}

fn get_raw<T: sap_flux_field_cli::link::Transport>(
    link: &mut Link<T>,
    a: GetRawArgs,
) -> Result<(), BoxError> {
    let run = raw::download(link, a.run, |bytes| {
        if bytes % 10_000 < 100 {
            eprint!("\r{} KiB", bytes / 1024);
        }
    })
    .map_err(|e| -> BoxError {
        match e {
            raw::RawDownloadError::Link(LinkError::NotFound) => format!(
                "no raw log for run {} (NOT_FOUND): never logged, or overwritten",
                a.run
            )
            .into(),
            other => other.into(),
        }
    })?;
    eprintln!();
    let frames = run.frames();
    println!(
        "run {}: {} blocks, {} frames, all block CRCs verified",
        run.run_id,
        run.blocks.len(),
        frames.len()
    );
    std::fs::create_dir_all(&a.out_dir)?;
    let csv = a.out_dir.join(format!("raw_{}.csv", run.run_id));
    let bin = a.out_dir.join(format!("raw_{}.bin", run.run_id));
    run.write_csv(std::io::BufWriter::new(std::fs::File::create(&csv)?))?;
    std::fs::write(&bin, run.bytes())?;
    println!("wrote {} and {}", csv.display(), bin.display());
    Ok(())
}

fn not_found_hint(e: LinkError) -> BoxError {
    match e {
        LinkError::NotFound => "no such window (NOT_FOUND); `status` shows windows_pending".into(),
        other => other.into(),
    }
}

