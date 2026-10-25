//! SQLite (WAL) storage for devices, config, check-ins, events, replay
//! state and window reassembly, plus one set of files per completed window.

use crate::board::{self, BoardMsg, FieldStatus};
use crate::config::{Config, Defaults};
use crate::window;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sapflux_uplink::{crc32, Schedule, WindowAck, WindowPart, SCHEDULE_FLAG_REQUIRE_LEASE};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Verification failures tolerated before a window is stored as `bad` and
/// acknowledged anyway (so the board can free it).
pub const MAX_WINDOW_FAILURES: i64 = 3;

/// Server-side event codes (gateway codes are 1–6).
pub const SERVER_EVENT_WINDOW_STORED: u16 = 1001;
pub const SERVER_EVENT_WINDOW_REJECTED: u16 = 1002;
pub const SERVER_EVENT_CONFIG_CHANGED: u16 = 1003;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS devices (
    device_id      INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    created_ms     INTEGER NOT NULL,
    last_seen_ms   INTEGER,
    last_peer      TEXT,
    last_boot_id   INTEGER,
    last_t1_ms     INTEGER,
    gw_fw          INTEGER,
    batt_mv        INTEGER,
    rsrp_dbm       INTEGER,
    uptime_s       INTEGER,
    checkin_flags  INTEGER,
    board_id       TEXT,
    board_seen_ms  INTEGER,
    board_json     TEXT,
    field_status   TEXT
);
CREATE TABLE IF NOT EXISTS device_config (
    device_id         INTEGER PRIMARY KEY,
    enabled           INTEGER NOT NULL,
    flags             INTEGER NOT NULL,
    pulse_duration_ms INTEGER NOT NULL,
    interval_s        INTEGER NOT NULL,
    phase_s           INTEGER NOT NULL,
    baseline_s        INTEGER NOT NULL,
    recovery_s        INTEGER NOT NULL,
    sample_hz         INTEGER NOT NULL,
    bin_ms            INTEGER NOT NULL,
    config_version    INTEGER NOT NULL,
    lease_s           INTEGER NOT NULL,
    wake_interval_s   INTEGER NOT NULL,
    send_windows      INTEGER NOT NULL,
    reboot_from_boot  INTEGER,
    updated_ms        INTEGER NOT NULL,
    updated_by        TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS config_log (
    id             INTEGER PRIMARY KEY,
    device_id      INTEGER NOT NULL,
    t_ms           INTEGER NOT NULL,
    actor          TEXT NOT NULL,
    config_version INTEGER NOT NULL,
    settings_json  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS replay (
    device_id   INTEGER NOT NULL,
    boot_id     INTEGER NOT NULL,
    last_seq    INTEGER NOT NULL,
    first_ms    INTEGER NOT NULL,
    last_ms     INTEGER NOT NULL,
    PRIMARY KEY (device_id, boot_id)
);
CREATE TABLE IF NOT EXISTS checkins (
    id            INTEGER PRIMARY KEY,
    device_id     INTEGER NOT NULL,
    t2_ms         INTEGER NOT NULL,
    boot_id       INTEGER NOT NULL,
    seq           INTEGER NOT NULL,
    t1_ms         INTEGER NOT NULL,
    gw_fw         INTEGER NOT NULL,
    batt_mv       INTEGER NOT NULL,
    rsrp_dbm      INTEGER NOT NULL,
    uptime_s      INTEGER NOT NULL,
    flags         INTEGER NOT NULL,
    peer          TEXT NOT NULL,
    board_frames  BLOB NOT NULL,
    board_json    TEXT NOT NULL,
    config_version INTEGER NOT NULL,
    in_sync       INTEGER
);
CREATE INDEX IF NOT EXISTS checkins_dev ON checkins(device_id, t2_ms);
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY,
    device_id   INTEGER,
    source      TEXT NOT NULL,
    boot_id     INTEGER,
    seq         INTEGER,
    t_unix_ms   INTEGER NOT NULL,
    received_ms INTEGER NOT NULL,
    code        INTEGER NOT NULL,
    text        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_dev ON events(device_id, received_ms);
CREATE TABLE IF NOT EXISTS window_partials (
    board_id    TEXT NOT NULL,
    run_id      INTEGER NOT NULL,
    device_id   INTEGER NOT NULL,
    part_count  INTEGER NOT NULL,
    total_len   INTEGER NOT NULL,
    started_ms  INTEGER NOT NULL,
    failures    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (board_id, run_id)
);
CREATE TABLE IF NOT EXISTS window_parts (
    board_id TEXT NOT NULL,
    run_id   INTEGER NOT NULL,
    part     INTEGER NOT NULL,
    data     BLOB NOT NULL,
    PRIMARY KEY (board_id, run_id, part)
);
CREATE TABLE IF NOT EXISTS windows (
    board_id          TEXT NOT NULL,
    run_id            INTEGER NOT NULL,
    generation        INTEGER NOT NULL,
    device_id         INTEGER NOT NULL,
    received_ms       INTEGER NOT NULL,
    status            TEXT NOT NULL,
    error             TEXT,
    total_len         INTEGER NOT NULL,
    part_count        INTEGER NOT NULL,
    part_crcs         BLOB NOT NULL,
    n_bins            INTEGER,
    window_flags      INTEGER,
    start_unix_ms     REAL,
    heater_on_unix_ms REAL,
    pulse_duration_ms INTEGER,
    sample_hz         INTEGER,
    bin_ms            INTEGER,
    frames_total      INTEGER,
    frames_missing    INTEGER,
    data_crc32        INTEGER,
    stem              TEXT NOT NULL,
    PRIMARY KEY (board_id, run_id, generation)
);
CREATE INDEX IF NOT EXISTS windows_recv ON windows(received_ms);
"#;

/// Per-device server config (what every CheckInReply carries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub device_id: u64,
    pub name: String,
    pub enabled: bool,
    pub require_lease: bool,
    pub pulse_duration_ms: u16,
    pub interval_s: u32,
    pub phase_s: u32,
    pub baseline_s: u16,
    pub recovery_s: u16,
    pub sample_hz: u16,
    pub bin_ms: u16,
    pub config_version: u32,
    pub lease_s: u32,
    pub wake_interval_s: u32,
    pub send_windows: bool,
    pub reboot_pending: bool,
    pub schedule_crc: u16,
    pub updated_ms: i64,
    pub updated_by: String,
}

impl Settings {
    /// Wire schedule.
    pub fn schedule(&self) -> Schedule {
        Schedule {
            enabled: self.enabled as u8,
            flags: if self.require_lease { SCHEDULE_FLAG_REQUIRE_LEASE } else { 0 },
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

/// Partial update of [`Settings`] (API `PUT .../schedule`, CLI).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    pub enabled: Option<bool>,
    pub require_lease: Option<bool>,
    pub pulse_duration_ms: Option<u16>,
    pub interval_s: Option<u32>,
    pub phase_s: Option<u32>,
    pub baseline_s: Option<u16>,
    pub recovery_s: Option<u16>,
    pub sample_hz: Option<u16>,
    pub bin_ms: Option<u16>,
    pub lease_s: Option<u32>,
    pub wake_interval_s: Option<u32>,
    pub send_windows: Option<bool>,
}

/// Device overview for `status`.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceView {
    pub device_id: u64,
    pub name: String,
    pub last_seen_ms: Option<i64>,
    pub last_peer: Option<String>,
    pub last_boot_id: Option<u32>,
    /// Server receive time minus gateway send time at the last check-in
    /// (gateway clock error plus uplink latency).
    pub last_t2_minus_t1_ms: Option<i64>,
    pub gw_fw: Option<u32>,
    pub batt_mv: Option<u16>,
    pub rsrp_dbm: Option<i16>,
    pub uptime_s: Option<u32>,
    pub checkin_flags: Option<u16>,
    pub board_id: Option<String>,
    pub board_seen_ms: Option<i64>,
    pub board: serde_json::Value,
    pub field_status: Option<FieldStatus>,
    pub field_status_flags: Vec<String>,
    /// Board's schedule_crc equals the server schedule's CRC.
    pub schedule_in_sync: Option<bool>,
    pub settings: Settings,
}

/// One check-in to record.
pub struct CheckinRecord<'a> {
    pub device_id: u64,
    pub peer: String,
    pub boot_id: u32,
    pub seq: u32,
    pub t1_ms: i64,
    pub t2_ms: i64,
    pub gw_fw: u32,
    pub batt_mv: u16,
    pub rsrp_dbm: i16,
    pub uptime_s: u32,
    pub flags: u16,
    pub board_frames: &'a [u8],
}

/// Stored event.
#[derive(Debug, Clone, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub device_id: Option<u64>,
    pub source: String,
    pub boot_id: Option<u32>,
    pub seq: Option<u32>,
    pub t_unix_ms: i64,
    pub received_ms: i64,
    pub code: u16,
    pub text: String,
}

/// Stored window.
#[derive(Debug, Clone, Serialize)]
pub struct WindowRow {
    pub board_id: String,
    pub run_id: u32,
    pub generation: u32,
    pub device_id: u64,
    pub received_ms: i64,
    pub status: String,
    pub error: Option<String>,
    pub total_len: u32,
    pub part_count: u16,
    pub n_bins: Option<u16>,
    pub window_flags: Option<u16>,
    pub flags: Vec<String>,
    pub start_unix_ms: Option<f64>,
    pub heater_on_unix_ms: Option<f64>,
    pub pulse_duration_ms: Option<u16>,
    pub sample_hz: Option<u16>,
    pub bin_ms: Option<u16>,
    pub frames_total: Option<u16>,
    pub frames_missing: Option<u16>,
    pub data_crc32: Option<u32>,
    /// Path of the files without extension (`.bin`, `.csv`, `.json`).
    pub stem: String,
}

/// Database handle plus the data directory.
pub struct Store {
    conn: Connection,
    data_dir: PathBuf,
}

fn now_ms() -> i64 {
    crate::now_ms()
}

impl Store {
    /// Open (creating) the database in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(data_dir.join("windows"))
            .with_context(|| format!("creating {}", data_dir.display()))?;
        let conn = Connection::open(data_dir.join("sapflux.db"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store {
            conn,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// Data directory.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Add devices from the config file (with `[defaults]` settings) that
    /// the database doesn't know yet, and refresh names.
    pub fn sync_devices(&self, cfg: &Config) -> Result<()> {
        for d in cfg.devices() {
            let id = d.device_id as i64;
            let known: bool = self
                .conn
                .query_row("SELECT 1 FROM devices WHERE device_id = ?1", [id], |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if known {
                self.conn
                    .execute("UPDATE devices SET name = ?2 WHERE device_id = ?1", params![id, d.name])?;
            } else {
                self.conn.execute(
                    "INSERT INTO devices (device_id, name, created_ms) VALUES (?1, ?2, ?3)",
                    params![id, d.name, now_ms()],
                )?;
                info!(device = %d.name, device_id = d.device_id, "new device from config");
            }
            let has_cfg: bool = self
                .conn
                .query_row("SELECT 1 FROM device_config WHERE device_id = ?1", [id], |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if !has_cfg {
                self.insert_default_settings(d.device_id, &cfg.defaults)?;
            }
        }
        Ok(())
    }

    fn insert_default_settings(&self, device_id: u64, d: &Defaults) -> Result<()> {
        let s = d.schedule();
        self.conn.execute(
            "INSERT INTO device_config (device_id, enabled, flags, pulse_duration_ms, interval_s,
                phase_s, baseline_s, recovery_s, sample_hz, bin_ms, config_version, lease_s,
                wake_interval_s, send_windows, reboot_from_boot, updated_ms, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?12, ?13, NULL, ?14, 'config-defaults')",
            params![
                device_id as i64,
                s.enabled,
                s.flags,
                s.pulse_duration_ms,
                s.interval_s,
                s.phase_s,
                s.baseline_s,
                s.recovery_s,
                s.sample_hz,
                s.bin_ms,
                d.lease_s,
                d.wake_interval_s,
                d.send_windows,
                now_ms()
            ],
        )?;
        self.log_config(device_id, "config-defaults")?;
        Ok(())
    }

    /// Resolve a device by name or decimal/hex id.
    pub fn resolve(&self, name_or_id: &str) -> Result<u64> {
        if let Some(id) = crate::config::parse_u64(name_or_id) {
            let found: Option<i64> = self
                .conn
                .query_row("SELECT device_id FROM devices WHERE device_id = ?1", [id as i64], |r| r.get(0))
                .optional()?;
            if found.is_some() {
                return Ok(id);
            }
        }
        self.conn
            .query_row("SELECT device_id FROM devices WHERE name = ?1", [name_or_id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            .map(|i| i as u64)
            .ok_or_else(|| anyhow!("unknown device {name_or_id:?}"))
    }

    // ---- replay ----------------------------------------------------------

    /// Accept `seq` for `(device_id, boot_id)` if it is greater than the
    /// last accepted one, recording it. Returns false for replays.
    pub fn accept_seq(&self, device_id: u64, boot_id: u32, seq: u32) -> Result<bool> {
        let now = now_ms();
        let last: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_seq FROM replay WHERE device_id = ?1 AND boot_id = ?2",
                params![device_id as i64, boot_id],
                |r| r.get(0),
            )
            .optional()?;
        match last {
            Some(l) if seq as i64 <= l => Ok(false),
            Some(_) => {
                self.conn.execute(
                    "UPDATE replay SET last_seq = ?3, last_ms = ?4 WHERE device_id = ?1 AND boot_id = ?2",
                    params![device_id as i64, boot_id, seq, now],
                )?;
                Ok(true)
            }
            None => {
                self.conn.execute(
                    "INSERT INTO replay (device_id, boot_id, last_seq, first_ms, last_ms) VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![device_id as i64, boot_id, seq, now],
                )?;
                Ok(true)
            }
        }
    }

    // ---- settings --------------------------------------------------------

    /// Current settings.
    pub fn settings(&self, device_id: u64) -> Result<Settings> {
        self.conn
            .query_row(
                "SELECT c.enabled, c.flags, c.pulse_duration_ms, c.interval_s, c.phase_s, c.baseline_s,
                        c.recovery_s, c.sample_hz, c.bin_ms, c.config_version, c.lease_s,
                        c.wake_interval_s, c.send_windows, c.reboot_from_boot, c.updated_ms,
                        c.updated_by, d.name
                 FROM device_config c JOIN devices d USING (device_id) WHERE device_id = ?1",
                [device_id as i64],
                |r| {
                    let flags: u8 = r.get(1)?;
                    let mut s = Settings {
                        device_id,
                        name: r.get(16)?,
                        enabled: r.get(0)?,
                        require_lease: flags & SCHEDULE_FLAG_REQUIRE_LEASE != 0,
                        pulse_duration_ms: r.get(2)?,
                        interval_s: r.get(3)?,
                        phase_s: r.get(4)?,
                        baseline_s: r.get(5)?,
                        recovery_s: r.get(6)?,
                        sample_hz: r.get(7)?,
                        bin_ms: r.get(8)?,
                        config_version: r.get(9)?,
                        lease_s: r.get(10)?,
                        wake_interval_s: r.get(11)?,
                        send_windows: r.get(12)?,
                        reboot_pending: r.get::<_, Option<i64>>(13)?.is_some(),
                        schedule_crc: 0,
                        updated_ms: r.get(14)?,
                        updated_by: r.get(15)?,
                    };
                    s.schedule_crc = s.schedule().crc16();
                    Ok(s)
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("no settings for device {device_id}"))
    }

    /// Apply a patch. Validates the schedule against the board's limits and
    /// bumps `config_version` if anything changed.
    pub fn update_settings(&self, device_id: u64, p: &SettingsPatch, actor: &str) -> Result<Settings> {
        let old = self.settings(device_id)?;
        let mut s = old.clone();
        macro_rules! apply {
            ($($f:ident),*) => { $( if let Some(v) = p.$f { s.$f = v; } )* };
        }
        apply!(
            enabled, require_lease, pulse_duration_ms, interval_s, phase_s, baseline_s, recovery_s,
            sample_hz, bin_ms, lease_s, wake_interval_s, send_windows
        );
        sapflux_uplink::validate_schedule(&s.schedule())
            .map_err(|e| anyhow!("schedule rejected: {}", sapflux_uplink::schedule_error_text(e)))?;
        if s.wake_interval_s < 60 {
            bail!("wake_interval_s must be >= 60");
        }
        if s == old {
            return Ok(old);
        }
        let sch = s.schedule();
        self.conn.execute(
            "UPDATE device_config SET enabled = ?2, flags = ?3, pulse_duration_ms = ?4, interval_s = ?5,
                phase_s = ?6, baseline_s = ?7, recovery_s = ?8, sample_hz = ?9, bin_ms = ?10,
                lease_s = ?11, wake_interval_s = ?12, send_windows = ?13,
                config_version = config_version + 1, updated_ms = ?14, updated_by = ?15
             WHERE device_id = ?1",
            params![
                device_id as i64,
                sch.enabled,
                sch.flags,
                sch.pulse_duration_ms,
                sch.interval_s,
                sch.phase_s,
                sch.baseline_s,
                sch.recovery_s,
                sch.sample_hz,
                sch.bin_ms,
                s.lease_s,
                s.wake_interval_s,
                s.send_windows,
                now_ms(),
                actor
            ],
        )?;
        let new = self.settings(device_id)?;
        self.log_config(device_id, actor)?;
        info!(
            device = %new.name, actor, enabled = new.enabled, config_version = new.config_version,
            "settings changed"
        );
        Ok(new)
    }

    /// Turn the schedule on or off (the remote off switch).
    pub fn set_enabled(&self, device_id: u64, enabled: bool, actor: &str) -> Result<Settings> {
        self.update_settings(
            device_id,
            &SettingsPatch {
                enabled: Some(enabled),
                ..Default::default()
            },
            actor,
        )
    }

    /// Bump `config_version` without other changes.
    pub fn bump_version(&self, device_id: u64, actor: &str) -> Result<Settings> {
        self.conn.execute(
            "UPDATE device_config SET config_version = config_version + 1, updated_ms = ?2, updated_by = ?3
             WHERE device_id = ?1",
            params![device_id as i64, now_ms(), actor],
        )?;
        self.log_config(device_id, actor)?;
        self.settings(device_id)
    }

    /// Ask the gateway to reboot: the flag is sent on every check-in until
    /// one arrives with a different `boot_id` than the current one.
    pub fn request_reboot(&self, device_id: u64, actor: &str) -> Result<()> {
        let boot: Option<i64> = self.conn.query_row(
            "SELECT last_boot_id FROM devices WHERE device_id = ?1",
            [device_id as i64],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "UPDATE device_config SET reboot_from_boot = ?2, updated_ms = ?3, updated_by = ?4 WHERE device_id = ?1",
            params![device_id as i64, boot.unwrap_or(-1), now_ms(), actor],
        )?;
        self.log_config(device_id, actor)?;
        Ok(())
    }

    /// Whether to set the reboot flag for a check-in from `boot_id`
    /// (clears the request once the gateway has rebooted).
    pub fn reboot_flag(&self, device_id: u64, boot_id: u32) -> Result<bool> {
        let from: Option<i64> = self.conn.query_row(
            "SELECT reboot_from_boot FROM device_config WHERE device_id = ?1",
            [device_id as i64],
            |r| r.get(0),
        )?;
        match from {
            None => Ok(false),
            Some(b) if b != boot_id as i64 && b != -1 => {
                self.conn.execute(
                    "UPDATE device_config SET reboot_from_boot = NULL WHERE device_id = ?1",
                    [device_id as i64],
                )?;
                info!(device_id, boot_id, "gateway rebooted; reboot request cleared");
                Ok(false)
            }
            Some(-1) => {
                // Requested before any check-in: pin it to this boot.
                self.conn.execute(
                    "UPDATE device_config SET reboot_from_boot = ?2 WHERE device_id = ?1",
                    params![device_id as i64, boot_id],
                )?;
                Ok(true)
            }
            Some(_) => Ok(true),
        }
    }

    fn log_config(&self, device_id: u64, actor: &str) -> Result<()> {
        let s = self.settings(device_id)?;
        self.conn.execute(
            "INSERT INTO config_log (device_id, t_ms, actor, config_version, settings_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![device_id as i64, now_ms(), actor, s.config_version, serde_json::to_string(&s)?],
        )?;
        Ok(())
    }

    /// Config change history, newest first.
    pub fn config_log(&self, device_id: u64, limit: u32) -> Result<Vec<serde_json::Value>> {
        let mut st = self.conn.prepare(
            "SELECT t_ms, actor, config_version, settings_json FROM config_log
             WHERE device_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![device_id as i64, limit], |r| {
            let settings: String = r.get(3)?;
            Ok(serde_json::json!({
                "t_ms": r.get::<_, i64>(0)?,
                "actor": r.get::<_, String>(1)?,
                "config_version": r.get::<_, i64>(2)?,
                "settings": serde_json::from_str::<serde_json::Value>(&settings).unwrap_or_default(),
            }))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // ---- check-ins and events -------------------------------------------

    /// Record a check-in and update the device's latest state. Returns the
    /// board's FieldStatus, if it was included.
    pub fn record_checkin(&self, c: &CheckinRecord<'_>, settings: &Settings) -> Result<Option<FieldStatus>> {
        let frames = board::parse_frames(c.board_frames);
        let mut board_id = None;
        let mut fs = None;
        let mut json = Vec::new();
        for f in &frames {
            match f {
                Ok(fr) => {
                    match &fr.msg {
                        BoardMsg::Hello(h) => board_id = Some(h.board_id.clone()),
                        BoardMsg::FieldStatus(s) => fs = Some(*s),
                        _ => {}
                    }
                    json.push(serde_json::to_value(fr)?);
                }
                Err(e) => {
                    warn!(device_id = c.device_id, error = %e, "bad board frame in check-in");
                    json.push(serde_json::json!({ "error": e.to_string() }));
                }
            }
        }
        let board_json = serde_json::Value::Array(json).to_string();
        let in_sync = fs.map(|f| f.schedule_crc == settings.schedule_crc);
        self.conn.execute(
            "INSERT INTO checkins (device_id, t2_ms, boot_id, seq, t1_ms, gw_fw, batt_mv, rsrp_dbm,
                uptime_s, flags, peer, board_frames, board_json, config_version, in_sync)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                c.device_id as i64,
                c.t2_ms,
                c.boot_id,
                c.seq,
                c.t1_ms,
                c.gw_fw,
                c.batt_mv,
                c.rsrp_dbm,
                c.uptime_s,
                c.flags,
                c.peer,
                c.board_frames,
                board_json,
                settings.config_version,
                in_sync
            ],
        )?;
        self.conn.execute(
            "UPDATE devices SET last_seen_ms = ?2, last_peer = ?3, last_boot_id = ?4, last_t1_ms = ?5,
                gw_fw = ?6, batt_mv = ?7, rsrp_dbm = ?8, uptime_s = ?9, checkin_flags = ?10
             WHERE device_id = ?1",
            params![
                c.device_id as i64,
                c.t2_ms,
                c.peer,
                c.boot_id,
                c.t1_ms,
                c.gw_fw,
                c.batt_mv,
                c.rsrp_dbm,
                c.uptime_s,
                c.flags
            ],
        )?;
        if !c.board_frames.is_empty() {
            self.conn.execute(
                "UPDATE devices SET board_seen_ms = ?2, board_json = ?3,
                    board_id = COALESCE(?4, board_id),
                    field_status = COALESCE(?5, field_status)
                 WHERE device_id = ?1",
                params![
                    c.device_id as i64,
                    c.t2_ms,
                    board_json,
                    board_id,
                    fs.map(|f| serde_json::to_string(&f)).transpose()?
                ],
            )?;
        }
        Ok(fs)
    }

    /// Store an event.
    #[allow(clippy::too_many_arguments)]
    pub fn record_event(
        &self,
        device_id: Option<u64>,
        source: &str,
        boot_id: Option<u32>,
        seq: Option<u32>,
        t_unix_ms: i64,
        code: u16,
        text: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (device_id, source, boot_id, seq, t_unix_ms, received_ms, code, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![device_id.map(|d| d as i64), source, boot_id, seq, t_unix_ms, now_ms(), code, text],
        )?;
        Ok(())
    }

    /// Events, newest first.
    pub fn events(&self, device_id: Option<u64>, limit: u32) -> Result<Vec<EventRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, device_id, source, boot_id, seq, t_unix_ms, received_ms, code, text FROM events
             WHERE ?1 IS NULL OR device_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![device_id.map(|d| d as i64), limit], |r| {
            Ok(EventRow {
                id: r.get(0)?,
                device_id: r.get::<_, Option<i64>>(1)?.map(|d| d as u64),
                source: r.get(2)?,
                boot_id: r.get(3)?,
                seq: r.get(4)?,
                t_unix_ms: r.get(5)?,
                received_ms: r.get(6)?,
                code: r.get(7)?,
                text: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// All devices with their latest state.
    pub fn devices(&self) -> Result<Vec<DeviceView>> {
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT device_id FROM devices ORDER BY name")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        ids.into_iter().map(|id| self.device(id as u64)).collect()
    }

    /// One device's latest state.
    pub fn device(&self, device_id: u64) -> Result<DeviceView> {
        let settings = self.settings(device_id)?;
        self.conn
            .query_row(
                "SELECT name, last_seen_ms, last_peer, last_boot_id, last_t1_ms, gw_fw, batt_mv, rsrp_dbm,
                        uptime_s, checkin_flags, board_id, board_seen_ms, board_json, field_status
                 FROM devices WHERE device_id = ?1",
                [device_id as i64],
                |r| {
                    let last_seen: Option<i64> = r.get(1)?;
                    let t1: Option<i64> = r.get(4)?;
                    let fs: Option<FieldStatus> = r
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok());
                    Ok(DeviceView {
                        device_id,
                        name: r.get(0)?,
                        last_seen_ms: last_seen,
                        last_peer: r.get(2)?,
                        last_boot_id: r.get(3)?,
                        last_t2_minus_t1_ms: last_seen.zip(t1).map(|(a, b)| a - b),
                        gw_fw: r.get(5)?,
                        batt_mv: r.get(6)?,
                        rsrp_dbm: r.get(7)?,
                        uptime_s: r.get(8)?,
                        checkin_flags: r.get(9)?,
                        board_id: r.get(10)?,
                        board_seen_ms: r.get(11)?,
                        board: r
                            .get::<_, Option<String>>(12)?
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or(serde_json::Value::Null),
                        field_status_flags: fs.map(|f| f.flag_names()).unwrap_or_default(),
                        schedule_in_sync: fs.map(|f| f.schedule_crc == settings.schedule_crc),
                        field_status: fs,
                        settings: settings.clone(),
                    })
                },
            )
            .context("device lookup")
    }

    // ---- windows ---------------------------------------------------------

    /// Store one WindowPart and return the WindowAck to send.
    pub fn handle_part(&self, device_id: u64, p: &WindowPart<'_>) -> Result<WindowAck> {
        let board = format!("{:016X}", p.board_id);
        let run = p.run_id as i64;
        let mut ack = WindowAck {
            board_id: p.board_id,
            run_id: p.run_id,
            complete: false,
            part_count: p.part_count,
            received_bitmap: [0; 32],
        };

        let partial: Option<(i64, i64, i64)> = self
            .conn
            .query_row(
                "SELECT part_count, total_len, failures FROM window_partials WHERE board_id = ?1 AND run_id = ?2",
                params![board, run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        if partial.is_none() {
            // Already complete? A resend of a stored part is acknowledged as
            // complete; different bytes mean the board reused the run id.
            let done: Option<(i64, i64, i64, Vec<u8>)> = self
                .conn
                .query_row(
                    "SELECT generation, part_count, total_len, part_crcs FROM windows
                     WHERE board_id = ?1 AND run_id = ?2 ORDER BY generation DESC LIMIT 1",
                    params![board, run],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((gen, pc, tl, crcs)) = done {
                let i = p.part as usize * 4;
                let same = pc == p.part_count as i64
                    && tl == p.total_len as i64
                    && crcs.get(i..i + 4) == Some(&crc32(p.data).to_le_bytes()[..]);
                if same {
                    ack.complete = true;
                    for i in 0..p.part_count {
                        ack.set_part(i);
                    }
                    return Ok(ack);
                }
                warn!(board = %board, run_id = p.run_id, generation = gen,
                      "part differs from the stored window: new window with a reused run_id");
            }
        }

        let failures = match partial {
            Some((pc, tl, f)) if pc == p.part_count as i64 && tl == p.total_len as i64 => f,
            Some(_) => {
                warn!(board = %board, run_id = p.run_id, "window geometry changed; restarting reassembly");
                self.drop_parts(&board, run)?;
                self.conn.execute(
                    "UPDATE window_partials SET part_count = ?3, total_len = ?4 WHERE board_id = ?1 AND run_id = ?2",
                    params![board, run, p.part_count, p.total_len],
                )?;
                0
            }
            None => {
                self.conn.execute(
                    "INSERT INTO window_partials (board_id, run_id, device_id, part_count, total_len, started_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![board, run, device_id as i64, p.part_count, p.total_len, now_ms()],
                )?;
                0
            }
        };

        self.conn.execute(
            "INSERT OR REPLACE INTO window_parts (board_id, run_id, part, data) VALUES (?1, ?2, ?3, ?4)",
            params![board, run, p.part, p.data],
        )?;

        let parts: Vec<(i64, Vec<u8>)> = self
            .conn
            .prepare("SELECT part, data FROM window_parts WHERE board_id = ?1 AND run_id = ?2 ORDER BY part")?
            .query_map(params![board, run], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (i, _) in &parts {
            ack.set_part(*i as u16);
        }
        if parts.len() < p.part_count as usize {
            return Ok(ack);
        }

        // All parts present: assemble and verify.
        let bytes: Vec<u8> = parts.iter().flat_map(|(_, d)| d.iter().copied()).collect();
        let part_crcs: Vec<u8> = parts.iter().flat_map(|(_, d)| crc32(d).to_le_bytes()).collect();
        let check = if bytes.len() != p.total_len as usize {
            Err(format!("reassembled {} bytes, total_len {}", bytes.len(), p.total_len))
        } else {
            window::verify(&bytes, Some(p.run_id)).map_err(|e| e.to_string())
        };
        match check {
            Ok((info, data)) => {
                let decoded = window::decode(&info, data);
                let row = self.store_window(device_id, &board, p, &bytes, &part_crcs, Ok(&decoded))?;
                info!(board = %board, run_id = p.run_id, generation = row.generation, n_bins = info.n_bins,
                      flags = ?row.flags, path = %row.stem, "window stored");
                self.record_event(
                    Some(device_id),
                    "server",
                    None,
                    None,
                    now_ms(),
                    SERVER_EVENT_WINDOW_STORED,
                    &format!("board {board} run {} stored ({})", p.run_id, window::size_text(&info)),
                )?;
                ack.complete = true;
            }
            Err(e) => {
                let failures = failures + 1;
                warn!(board = %board, run_id = p.run_id, failures, error = %e, "window verification failed");
                self.record_event(
                    Some(device_id),
                    "server",
                    None,
                    None,
                    now_ms(),
                    SERVER_EVENT_WINDOW_REJECTED,
                    &format!("board {board} run {} failure {failures}: {e}", p.run_id),
                )?;
                if failures >= MAX_WINDOW_FAILURES {
                    let row = self.store_window(device_id, &board, p, &bytes, &part_crcs, Err(&e))?;
                    warn!(board = %board, run_id = p.run_id, path = %row.stem,
                          "giving up: stored raw bytes as bad and acknowledged");
                    ack.complete = true;
                } else {
                    self.drop_parts(&board, run)?;
                    self.conn.execute(
                        "UPDATE window_partials SET failures = ?3 WHERE board_id = ?1 AND run_id = ?2",
                        params![board, run, failures],
                    )?;
                    ack.received_bitmap = [0; 32];
                }
            }
        }
        Ok(ack)
    }

    fn drop_parts(&self, board: &str, run: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM window_parts WHERE board_id = ?1 AND run_id = ?2",
            params![board, run],
        )?;
        Ok(())
    }

    fn store_window(
        &self,
        device_id: u64,
        board: &str,
        p: &WindowPart<'_>,
        bytes: &[u8],
        part_crcs: &[u8],
        decoded: Result<&window::Decoded, &String>,
    ) -> Result<WindowRow> {
        let run = p.run_id as i64;
        let generation: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(generation) + 1, 0) FROM windows WHERE board_id = ?1 AND run_id = ?2",
            params![board, run],
            |r| r.get(0),
        )?;
        let dir = self.data_dir.join("windows").join(board);
        std::fs::create_dir_all(&dir)?;
        let name = if generation == 0 {
            format!("run_{}", p.run_id)
        } else {
            format!("run_{}_g{generation}", p.run_id)
        };
        let stem = dir.join(&name);
        write_atomic(&stem.with_extension("bin"), bytes)?;
        let (status, error) = match decoded {
            Ok(d) => {
                let mut csv = Vec::new();
                window::write_csv(d, &mut csv)?;
                write_atomic(&stem.with_extension("csv"), &csv)?;
                let mut js = serde_json::to_value(d.summary())?;
                js["board_id"] = board.into();
                js["device_id"] = device_id.into();
                js["generation"] = generation.into();
                write_atomic(&stem.with_extension("json"), serde_json::to_string_pretty(&js)?.as_bytes())?;
                ("ok", None)
            }
            Err(e) => ("bad", Some(e.clone())),
        };
        let info = decoded.ok().map(|d| (d.info, d.summary()));
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO windows (board_id, run_id, generation, device_id, received_ms, status, error,
                total_len, part_count, part_crcs, n_bins, window_flags, start_unix_ms, heater_on_unix_ms,
                pulse_duration_ms, sample_hz, bin_ms, frames_total, frames_missing, data_crc32, stem)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                board,
                run,
                generation,
                device_id as i64,
                now_ms(),
                status,
                error,
                p.total_len,
                p.part_count,
                part_crcs,
                info.as_ref().map(|i| i.0.n_bins),
                info.as_ref().map(|i| i.0.window_flags),
                info.as_ref().and_then(|i| i.1.start_unix_ms),
                info.as_ref().and_then(|i| i.1.heater_on_unix_ms),
                info.as_ref().map(|i| i.0.pulse_duration_ms),
                info.as_ref().map(|i| i.0.sample_hz),
                info.as_ref().map(|i| i.0.bin_ms() as i64),
                info.as_ref().map(|i| i.0.frames_total),
                info.as_ref().map(|i| i.0.frames_missing),
                info.as_ref().map(|i| i.0.data_crc32),
                stem.to_string_lossy()
            ],
        )?;
        tx.execute("DELETE FROM window_parts WHERE board_id = ?1 AND run_id = ?2", params![board, run])?;
        tx.execute("DELETE FROM window_partials WHERE board_id = ?1 AND run_id = ?2", params![board, run])?;
        tx.commit()?;
        self.window(board, p.run_id, Some(generation as u32))?
            .context("window just stored")
    }

    /// Stored windows, newest first, optionally for one board.
    pub fn windows(&self, board: Option<&str>, limit: u32) -> Result<Vec<WindowRow>> {
        let board = board.map(|b| b.to_uppercase());
        let mut st = self.conn.prepare(&format!(
            "{WINDOW_SELECT} WHERE ?1 IS NULL OR board_id = ?1 ORDER BY received_ms DESC, run_id DESC LIMIT ?2"
        ))?;
        let rows = st.query_map(params![board, limit], window_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One stored window (latest generation unless given).
    pub fn window(&self, board: &str, run_id: u32, generation: Option<u32>) -> Result<Option<WindowRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "{WINDOW_SELECT} WHERE board_id = ?1 AND run_id = ?2 AND (?3 IS NULL OR generation = ?3)
                     ORDER BY generation DESC LIMIT 1"
                ),
                params![board.to_uppercase(), run_id, generation],
                window_row,
            )
            .optional()?)
    }

    /// Windows still being reassembled.
    pub fn partials(&self) -> Result<Vec<serde_json::Value>> {
        let mut st = self.conn.prepare(
            "SELECT p.board_id, p.run_id, p.device_id, p.part_count, p.total_len, p.started_ms, p.failures,
                    (SELECT COUNT(*) FROM window_parts w WHERE w.board_id = p.board_id AND w.run_id = p.run_id)
             FROM window_partials p ORDER BY started_ms DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(serde_json::json!({
                "board_id": r.get::<_, String>(0)?,
                "run_id": r.get::<_, i64>(1)?,
                "device_id": r.get::<_, i64>(2)? as u64,
                "part_count": r.get::<_, i64>(3)?,
                "total_len": r.get::<_, i64>(4)?,
                "started_ms": r.get::<_, i64>(5)?,
                "failures": r.get::<_, i64>(6)?,
                "parts_received": r.get::<_, i64>(7)?,
            }))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

const WINDOW_SELECT: &str = "SELECT board_id, run_id, generation, device_id, received_ms, status, error,
    total_len, part_count, n_bins, window_flags, start_unix_ms, heater_on_unix_ms, pulse_duration_ms,
    sample_hz, bin_ms, frames_total, frames_missing, data_crc32, stem FROM windows";

fn window_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<WindowRow> {
    let flags: Option<u16> = r.get(10)?;
    Ok(WindowRow {
        board_id: r.get(0)?,
        run_id: r.get(1)?,
        generation: r.get(2)?,
        device_id: r.get::<_, i64>(3)? as u64,
        received_ms: r.get(4)?,
        status: r.get(5)?,
        error: r.get(6)?,
        total_len: r.get(7)?,
        part_count: r.get(8)?,
        n_bins: r.get(9)?,
        window_flags: flags,
        flags: flags.map(board::window_flag_names).unwrap_or_default(),
        start_unix_ms: r.get(11)?,
        heater_on_unix_ms: r.get(12)?,
        pulse_duration_ms: r.get(13)?,
        sample_hz: r.get(14)?,
        bin_ms: r.get(15)?,
        frames_total: r.get(16)?,
        frames_missing: r.get(17)?,
        data_crc32: r.get(18)?,
        stem: r.get(19)?,
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
