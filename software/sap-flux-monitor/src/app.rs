//! The egui monitor application.
//!
//! Layout: a dense instrument screen — top connection strip, right-hand
//! status/values/pulse-control column, central live plots. No landing page.

use crate::channels::{CHANNELS, CHANNEL_COUNT, EXCITATION_COLOR};
use crate::control::{
    PulseControl, BASELINE_S_RANGE, PULSE_CHOICES_MS, RECOVERY_S_RANGE, SAMPLE_HZ_RANGE,
};
use crate::history::SeriesBuf;
use crate::stats::StatsSnapshot;
use crate::types::{format_fw_version, host_unix_ns, state_name, ProcessedSample};
use crate::worker::{self, ConnectionTarget, UiEvent, WorkerCmd, WorkerHandle};
use eframe::egui::{self, Color32, RichText, Ui};
use sap_flux_protocol::{
    Abort, ArmPulse, BoardState, FirePulse, Hello, Message, Query, SetRtc, StartBaseline, Status,
    FLAG_HEATER_ACTIVE, FLAG_PGOOD, MSG_SET_RTC, PROTOCOL_VERSION,
};
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The adapter we expect on this bench; pinned to the top of the port list
/// and auto-selected when present.
const PREFERRED_PORT: &str = "/dev/cu.usbserial-BG02Q2RS";
const DEFAULT_BAUD: u32 = 115_200;

/// Selectable plot history windows (seconds).
const WINDOW_CHOICES: [(f64, &str); 4] = [
    (30.0, "30 s"),
    (60.0, "1 min"),
    (300.0, "5 min"),
    (900.0, "15 min"),
];

struct Connection {
    handle: WorkerHandle,
    name: String,
    simulated: bool,
}

struct FireModal {
    run_id: u32,
    pulse_ms: u16,
    nonce: u64,
    opened: Instant,
}

/// Everything shown for the live board.
#[derive(Default)]
struct BoardView {
    hello: Option<Hello>,
    status: Option<Status>,
    status_at: Option<Instant>,
    latest: Option<Box<ProcessedSample>>,
}

/// The monitor app.
pub struct MonitorApp {
    conn: Option<Connection>,
    ports: Vec<String>,
    selected_port: Option<String>,
    sim_inject_errors: bool,

    board: BoardView,
    stats: StatsSnapshot,
    session_dir: Option<PathBuf>,
    log_error: Option<String>,
    last_decode_error: Option<String>,
    disconnect_reason: Option<String>,
    last_command: Option<String>,

    temp_series: [SeriesBuf; CHANNEL_COUNT],
    raw_series: [SeriesBuf; CHANNEL_COUNT],
    exc_series: SeriesBuf,
    show_channel: [bool; CHANNEL_COUNT],
    show_raw_plot: bool,
    window_secs: f64,

    control: PulseControl,
    fire_modal: Option<FireModal>,
    rtc_pending_nonce: Option<u64>,
    rtc_queued: bool,
    rtc_last_result: Option<String>,
    /// Worker-reported count of commands waiting for a command window.
    queue_depth: usize,

    nonce_counter: u64,
    captures_root: PathBuf,
}

impl MonitorApp {
    /// Create the app; `captures_root` is where sessions are stored.
    pub fn new(captures_root: PathBuf) -> Self {
        let mut show_channel = [true; CHANNEL_COUNT];
        for (i, c) in CHANNELS.iter().enumerate() {
            // The known-broken channel is labeled but hidden by default. It
            // remains available in raw data and can be shown explicitly.
            if c.known_broken {
                show_channel[i] = false;
            }
        }
        Self {
            conn: None,
            ports: Vec::new(),
            selected_port: None,
            sim_inject_errors: false,
            board: BoardView::default(),
            stats: StatsSnapshot::default(),
            session_dir: None,
            log_error: None,
            last_decode_error: None,
            disconnect_reason: None,
            last_command: None,
            temp_series: Default::default(),
            raw_series: Default::default(),
            exc_series: SeriesBuf::default(),
            show_channel,
            show_raw_plot: false,
            window_secs: 300.0,
            control: PulseControl::default(),
            fire_modal: None,
            rtc_pending_nonce: None,
            rtc_queued: false,
            rtc_last_result: None,
            queue_depth: 0,
            nonce_counter: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(1),
            captures_root,
        }
    }

    fn next_nonce(&mut self) -> u64 {
        self.nonce_counter = self.nonce_counter.wrapping_add(1);
        self.nonce_counter
    }

    fn refresh_ports(&mut self) {
        let mut names: Vec<String> = serialport::available_ports()
            .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
            .unwrap_or_default();
        names.sort_by(|a, b| {
            // Preferred adapter first, then callout devices, then the rest.
            let rank = |n: &str| {
                if n == PREFERRED_PORT {
                    0
                } else if n.starts_with("/dev/cu.") {
                    1
                } else {
                    2
                }
            };
            rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
        });
        if let Some(sel) = &self.selected_port {
            if !names.contains(sel) {
                self.selected_port = None;
            }
        }
        if self.selected_port.is_none() {
            self.selected_port = names
                .iter()
                .find(|n| n.as_str() == PREFERRED_PORT)
                .or_else(|| names.first())
                .cloned();
        }
        self.ports = names;
    }

    fn connect(&mut self, ctx: &egui::Context, target: ConnectionTarget) {
        self.disconnect();
        self.reset_stream_state();
        let name = target.name();
        let simulated = matches!(target, ConnectionTarget::Simulator { .. });
        for (visible, info) in self.show_channel.iter_mut().zip(&CHANNELS) {
            *visible = simulated || !info.known_broken;
        }
        let handle = worker::spawn(target, self.captures_root.clone(), Some(ctx.clone()));
        self.conn = Some(Connection {
            handle,
            name,
            simulated,
        });
        self.disconnect_reason = None;
    }

    fn disconnect(&mut self) {
        if let Some(mut c) = self.conn.take() {
            c.handle.shutdown();
        }
        self.on_link_down();
    }

    /// Clear per-connection state; called for both user disconnects and
    /// link failures. All armed/fire intent is dropped.
    fn on_link_down(&mut self) {
        self.control.on_disconnect();
        self.fire_modal = None;
        self.rtc_pending_nonce = None;
        self.rtc_queued = false;
        // Unsent queued commands are dropped with the worker, never
        // carried across a reconnect.
        self.queue_depth = 0;
        self.board = BoardView::default();
    }

    fn reset_stream_state(&mut self) {
        for s in &mut self.temp_series {
            s.clear();
        }
        for s in &mut self.raw_series {
            s.clear();
        }
        self.exc_series.clear();
        self.stats = StatsSnapshot::default();
        self.session_dir = None;
        self.log_error = None;
        self.last_decode_error = None;
        self.last_command = None;
        self.rtc_last_result = None;
    }

    fn send(&mut self, message: Message, desc: String) {
        if let Some(c) = &self.conn {
            // Immediate feedback: on a real half-duplex link the worker
            // queues the command until the board opens a command window;
            // only the simulator consumes it right away.
            self.last_command = Some(if c.simulated {
                desc.clone()
            } else {
                format!("{desc} — queued for next command window")
            });
            let _ = c.handle.cmd_tx.send(WorkerCmd::Send { message, desc });
        }
    }

    fn drain_events(&mut self) {
        let now = Instant::now();
        let mut link_down: Option<Option<String>> = None;
        // Collect first: handling an event needs &mut self, but the
        // receiver lives inside self.conn.
        let mut pending = Vec::new();
        if let Some(c) = &self.conn {
            loop {
                match c.handle.ui_rx.try_recv() {
                    Ok(ev) => {
                        pending.push(ev);
                        if pending.len() > 100_000 {
                            break; // safety valve; the rest next frame
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        link_down = Some(Some("worker stopped".into()));
                        break;
                    }
                }
            }
        }
        for ev in pending {
            match ev {
                UiEvent::Connected { port, simulated } => {
                    if let Some(c) = &mut self.conn {
                        c.name = port;
                        c.simulated = simulated;
                    }
                }
                UiEvent::Disconnected { reason } => {
                    link_down = Some(reason);
                }
                UiEvent::HelloMsg(h) => {
                    self.board.hello = Some(h);
                    self.control.note_hello(&h);
                }
                UiEvent::StatusMsg(st) => {
                    self.board.status = Some(st);
                    self.board.status_at = Some(now);
                    self.control.note_status(st, now);
                }
                UiEvent::AckMsg(a) => {
                    self.control.note_ack(&a);
                    if self.rtc_pending_nonce == Some(a.command_nonce)
                        && a.command_type == MSG_SET_RTC
                    {
                        self.rtc_pending_nonce = None;
                        self.rtc_last_result = Some(if a.result == 0 {
                            "RTC set: accepted".to_string()
                        } else {
                            format!("RTC set: rejected (code {})", a.result)
                        });
                    }
                }
                UiEvent::ErrorMsg(e) => {
                    self.last_decode_error = Some(format!(
                        "board error: fault={:#06x} detail={:#06x}",
                        e.fault_code, e.detail
                    ));
                }
                UiEvent::SampleMsg(ps) => {
                    self.control
                        .note_sample(ps.sample.flags, ps.sample_valid, now);
                    self.push_sample(&ps);
                    self.board.latest = Some(ps);
                }
                UiEvent::DecodeError(d) => self.last_decode_error = Some(d),
                UiEvent::SessionStarted(dir) => self.session_dir = Some(dir),
                UiEvent::LogError(e) => self.log_error = Some(e),
                UiEvent::Stats(s) => self.stats = s,
                UiEvent::CommandSent(desc) => {
                    self.queue_depth = self.queue_depth.saturating_sub(1);
                    if desc.starts_with("SetRtc") {
                        self.rtc_queued = false;
                    }
                    self.last_command = Some(format!("{desc} — sent"));
                }
                UiEvent::CommandQueued { desc, depth } => {
                    self.queue_depth = depth;
                    self.last_command = Some(format!("{desc} — queued for next command window"));
                }
            }
        }
        if let Some(reason) = link_down {
            if let Some(mut c) = self.conn.take() {
                c.handle.shutdown();
            }
            self.disconnect_reason = reason;
            self.on_link_down();
        }
    }

    fn push_sample(&mut self, ps: &ProcessedSample) {
        let t = ps.t_rel_s;
        for (i, r) in ps.readings.iter().enumerate() {
            if let Some(temp) = r.temp_c {
                self.temp_series[i].push(t, temp);
            }
            self.raw_series[i].push(t, r.raw as f64);
        }
        self.exc_series.push(t, ps.sample.excitation_raw as f64);
    }

    // ----- UI pieces -------------------------------------------------

    fn top_bar(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        ui.horizontal(|ui| {
            match &self.conn {
                None => {
                    if self.ports.is_empty() {
                        self.refresh_ports();
                    }
                    let selected = self
                        .selected_port
                        .clone()
                        .unwrap_or_else(|| "no port".into());
                    egui::ComboBox::from_id_salt("port_select")
                        .width(260.0)
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for p in self.ports.clone() {
                                let label = if p == PREFERRED_PORT {
                                    format!("{p}  (sap-flux adapter)")
                                } else {
                                    p.clone()
                                };
                                ui.selectable_value(&mut self.selected_port, Some(p), label);
                            }
                        });
                    if ui.button("⟳").on_hover_text("Refresh port list").clicked() {
                        self.refresh_ports();
                    }
                    ui.label(RichText::new(format!("{DEFAULT_BAUD} 8N1")).weak());
                    let can_connect = self.selected_port.is_some();
                    if ui
                        .add_enabled(can_connect, egui::Button::new("Connect"))
                        .clicked()
                    {
                        if let Some(port) = self.selected_port.clone() {
                            self.connect(
                                ctx,
                                ConnectionTarget::Serial {
                                    port,
                                    baud: DEFAULT_BAUD,
                                },
                            );
                        }
                    }
                    ui.separator();
                    if ui.button("Simulator").clicked() {
                        let inject_errors = self.sim_inject_errors;
                        self.connect(ctx, ConnectionTarget::Simulator { inject_errors });
                    }
                    ui.checkbox(&mut self.sim_inject_errors, "inject errors")
                        .on_hover_text("Simulator only: periodic CRC errors and sequence gaps");
                    if let Some(r) = &self.disconnect_reason {
                        ui.separator();
                        ui.label(RichText::new(format!("link lost: {r}")).color(Color32::RED));
                    }
                }
                Some(c) => {
                    let dot = if c.simulated {
                        RichText::new("● SIM").color(Color32::from_rgb(160, 100, 220))
                    } else {
                        RichText::new("● LIVE").color(Color32::from_rgb(0, 150, 70))
                    };
                    ui.label(dot.strong());
                    ui.label(RichText::new(&c.name).monospace());
                    if !c.simulated {
                        ui.label(RichText::new(format!("{DEFAULT_BAUD} 8N1")).weak());
                    }
                    if ui.button("Disconnect").clicked() {
                        self.disconnect();
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(e) = &self.log_error {
                    ui.label(
                        RichText::new(format!("LOG ERROR: {e}"))
                            .color(Color32::RED)
                            .strong(),
                    );
                } else if let Some(dir) = &self.session_dir {
                    ui.label(
                        RichText::new(format!("capturing → {}", dir.display()))
                            .weak()
                            .small(),
                    );
                } else if self.conn.is_some() {
                    ui.label(RichText::new("waiting for first frame…").weak().small());
                }
            });
        });
    }

    fn side_panel(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.board_section(ui, now);
            ui.separator();
            self.rtc_section(ui);
            ui.separator();
            self.link_section(ui);
            ui.separator();
            self.values_section(ui);
            ui.separator();
            self.pulse_section(ui, now);
        });
    }

    fn board_section(&mut self, ui: &mut Ui, _now: Instant) {
        ui.label(RichText::new("Board").strong());
        egui::Grid::new("board_grid")
            .num_columns(2)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                ui.label("connection");
                match &self.conn {
                    Some(c) if c.simulated => ui.label("simulator"),
                    Some(c) => ui.label(c.name.clone()),
                    None => ui.label(RichText::new("disconnected").color(Color32::GRAY)),
                };
                ui.end_row();
                ui.label("protocol");
                ui.label(format!("v{PROTOCOL_VERSION}"));
                ui.end_row();
                ui.label("board id");
                match &self.board.hello {
                    Some(h) => ui.label(RichText::new(format!("{:016X}", h.board_id)).monospace()),
                    None => ui.label("—"),
                };
                ui.end_row();
                ui.label("firmware");
                match &self.board.hello {
                    Some(h) => ui.label(format_fw_version(h.firmware_version)),
                    None => ui.label("—"),
                };
                ui.end_row();
                ui.label("state");
                match &self.board.status {
                    Some(st) => {
                        ui.label(
                            RichText::new(state_name(st.state))
                                .color(state_color(st.state))
                                .strong(),
                        );
                    }
                    None => {
                        ui.label("—");
                    }
                }
                ui.end_row();
                ui.label("run id");
                match &self.board.status {
                    Some(st) if st.run_id != 0 => ui.label(format!("{}", st.run_id)),
                    Some(_) => ui.label("idle"),
                    None => ui.label("—"),
                };
                ui.end_row();
                if let Some(st) = &self.board.status {
                    if st.fault_code != 0 {
                        ui.label(RichText::new("fault").color(Color32::RED).strong());
                        ui.label(
                            RichText::new(format!("code {}", st.fault_code)).color(Color32::RED),
                        );
                        ui.end_row();
                    }
                }
            });
    }

    fn rtc_section(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("RTC").strong());
        let rtc_ms = self
            .board
            .latest
            .as_ref()
            .map(|p| p.sample.rtc_unix_ms)
            .or(self.board.status.as_ref().map(|s| s.rtc_unix_ms));
        match rtc_ms {
            Some(ms) if ms >= 0 => {
                let text = format_rtc_ms(ms);
                ui.label(RichText::new("valid").color(Color32::from_rgb(0, 140, 60)));
                ui.label(RichText::new(text).monospace());
            }
            Some(_) => {
                ui.label(
                    RichText::new("INVALID — board clock not set")
                        .color(Color32::from_rgb(200, 120, 0))
                        .strong(),
                );
            }
            None => {
                ui.label("—");
            }
        }
        ui.horizontal(|ui| {
            let enabled = self.conn.is_some() && self.rtc_pending_nonce.is_none();
            if ui
                .add_enabled(enabled, egui::Button::new("Set RTC from computer"))
                .on_hover_text(
                    "Sends an explicit SetRtc command. Never sent automatically. \
                     On a real link it transmits at the next command window.",
                )
                .clicked()
            {
                let nonce = self.next_nonce();
                let unix_ms = host_unix_ns() / 1_000_000;
                self.rtc_pending_nonce = Some(nonce);
                self.rtc_queued = self.conn.as_ref().is_some_and(|c| !c.simulated);
                self.rtc_last_result = None;
                self.send(
                    Message::SetRtc(SetRtc {
                        command_nonce: nonce,
                        unix_ms,
                    }),
                    format!("SetRtc unix_ms={unix_ms} nonce={nonce}"),
                );
            }
            if self.rtc_pending_nonce.is_some() {
                if self.rtc_queued {
                    ui.label(
                        RichText::new("Queued for next command window…")
                            .color(Color32::from_rgb(200, 120, 0)),
                    );
                } else {
                    ui.label(RichText::new("waiting for ack…").weak());
                }
            } else if let Some(r) = &self.rtc_last_result {
                ui.label(RichText::new(r).weak());
            }
        });
    }

    fn link_section(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Link").strong());
        let s = &self.stats;
        egui::Grid::new("link_grid")
            .num_columns(2)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                ui.label("sample rate");
                ui.label(format!("{:.1} Hz", s.sample_rate_hz));
                ui.end_row();
                ui.label("frames / samples");
                ui.label(format!("{} / {}", s.frames_ok, s.samples_ok));
                ui.end_row();
                ui.label("seq gaps (missing)");
                let gap_text = format!("{} ({})", s.seq_gaps, s.seq_missing);
                if s.seq_gaps > 0 {
                    ui.label(RichText::new(gap_text).color(Color32::from_rgb(200, 120, 0)));
                } else {
                    ui.label(gap_text);
                }
                ui.end_row();
                ui.label("crc / framing errors");
                let err_text = format!("{} / {}", s.crc_errors, s.framing_errors);
                if s.crc_errors + s.framing_errors > 0 {
                    ui.label(RichText::new(err_text).color(Color32::from_rgb(200, 120, 0)));
                } else {
                    ui.label(err_text);
                }
                ui.end_row();
                ui.label("invalid samples");
                ui.label(format!("{}", s.invalid_samples));
                ui.end_row();
                ui.label("bytes rx / tx");
                ui.label(format!("{} / {}", s.bytes_rx, s.bytes_tx));
                ui.end_row();
                ui.label("UI events dropped");
                let drop_text = format!("{}", s.ui_events_dropped);
                if s.ui_events_dropped > 0 {
                    ui.label(
                        RichText::new(format!("{drop_text} (display only; log unaffected)"))
                            .color(Color32::from_rgb(200, 120, 0)),
                    );
                } else {
                    ui.label(drop_text);
                }
                ui.end_row();
            });
        if let Some(e) = &self.last_decode_error {
            ui.label(RichText::new(format!("last error: {e}")).weak().small());
        }
    }

    fn values_section(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Current values").strong());
        let Some(ps) = self.board.latest.as_deref() else {
            ui.label(RichText::new("no samples yet").weak());
            return;
        };
        let smp = &ps.sample;
        egui::Grid::new("values_grid")
            .num_columns(4)
            .spacing([10.0, 2.0])
            .show(ui, |ui| {
                ui.label(RichText::new("ch").weak().small());
                ui.label(RichText::new("temp").weak().small());
                ui.label(RichText::new("raw").weak().small());
                ui.label(RichText::new("").weak().small());
                ui.end_row();
                for (i, (info, r)) in CHANNELS.iter().zip(&ps.readings).enumerate() {
                    let _ = i;
                    let known_broken = info.known_broken
                        && !self
                            .conn
                            .as_ref()
                            .is_some_and(|connection| connection.simulated);
                    let mut name = RichText::new(format!("● {}", info.id)).color(info.color);
                    if known_broken {
                        name = name.strikethrough();
                    }
                    ui.label(name).on_hover_text(format!(
                        "{} wire — {}{}",
                        info.wire_color,
                        info.position,
                        if known_broken {
                            " (known broken on current probe)"
                        } else {
                            ""
                        }
                    ));
                    match r.temp_c {
                        Some(t) => ui.label(RichText::new(format!("{t:7.3} °C")).monospace()),
                        None => ui.label(
                            RichText::new(if known_broken { "broken" } else { "invalid" })
                                .color(Color32::from_rgb(200, 120, 0)),
                        ),
                    };
                    ui.label(RichText::new(format!("{}", r.raw)).monospace().weak());
                    match r.resistance_ohm {
                        Some(res) => ui.label(
                            RichText::new(format!("{:.0} Ω", res))
                                .monospace()
                                .weak()
                                .small(),
                        ),
                        None => ui.label(""),
                    };
                    ui.end_row();
                }
            });
        egui::Grid::new("values_grid2")
            .num_columns(2)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                ui.label("excitation raw");
                let exc_text = format!(
                    "{}{}",
                    smp.excitation_raw,
                    if ps.exc_valid { "" } else { "  (invalid)" }
                );
                ui.label(RichText::new(exc_text).monospace());
                ui.end_row();
                ui.label("24 V monitor raw");
                ui.label(RichText::new(format!("{}", smp.v24_raw)).monospace());
                ui.end_row();
                ui.label("PGOOD");
                bool_badge(ui, smp.flags & FLAG_PGOOD != 0, "good", "NOT GOOD");
                ui.end_row();
                ui.label("heater");
                if smp.flags & FLAG_HEATER_ACTIVE != 0 {
                    ui.label(RichText::new("ACTIVE").color(Color32::RED).strong());
                } else {
                    ui.label("off");
                }
                ui.end_row();
                ui.label("board state");
                ui.label(
                    RichText::new(state_name(smp.state))
                        .color(state_color(smp.state))
                        .strong(),
                );
                ui.end_row();
            });
    }

    fn pulse_section(&mut self, ui: &mut Ui, now: Instant) {
        ui.label(RichText::new("Pulse control").strong());
        ui.label(
            RichText::new("Firmware is the safety authority; these controls only make requests.")
                .weak()
                .small(),
        );

        // The board's Hello did not claim heater control: Start/Arm/Fire
        // stay disabled (can_* return false); say why. Monitoring, RTC,
        // plotting, logging — and Abort while the board reports an active
        // run — are unaffected.
        if self.control.heater_disabled_by_firmware() {
            ui.label(
                RichText::new("Heater disabled by firmware")
                    .color(Color32::from_rgb(200, 120, 0))
                    .strong(),
            )
            .on_hover_text(
                "The board's Hello does not advertise the heater-control \
                 capability. Start baseline, Arm, and Fire are unavailable.",
            );
        }

        // Board-reported cooldown, extrapolated for display only.
        if let Some(st) = self.control.last_status() {
            if st.cooldown_remaining_ms > 0 {
                let elapsed_ms = self
                    .board
                    .status_at
                    .map(|at| now.duration_since(at).as_millis() as u32)
                    .unwrap_or(0);
                let remaining = st.cooldown_remaining_ms.saturating_sub(elapsed_ms);
                ui.label(
                    RichText::new(format!(
                        "COOLDOWN {:02}:{:02} (board-reported)",
                        remaining / 60_000,
                        (remaining % 60_000) / 1000
                    ))
                    .color(Color32::from_rgb(150, 80, 200))
                    .strong(),
                );
            }
        }

        egui::Grid::new("baseline_grid")
            .num_columns(2)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                ui.label("baseline");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.control.params.baseline_s)
                            .range(BASELINE_S_RANGE)
                            .suffix(" s"),
                    );
                });
                ui.end_row();
                ui.label("recovery");
                ui.add(
                    egui::DragValue::new(&mut self.control.params.recovery_s)
                        .range(RECOVERY_S_RANGE)
                        .suffix(" s"),
                );
                ui.end_row();
                ui.label("sample rate");
                ui.add(
                    egui::DragValue::new(&mut self.control.params.sample_hz)
                        .range(SAMPLE_HZ_RANGE)
                        .suffix(" Hz"),
                );
                ui.end_row();
                ui.label("pulse duration");
                ui.horizontal(|ui| {
                    for ms in PULSE_CHOICES_MS {
                        ui.selectable_value(
                            &mut self.control.pulse_ms,
                            ms,
                            format!("{:.1} s", f64::from(ms) / 1000.0),
                        );
                    }
                });
                ui.end_row();
            });
        self.control.params.clamp();

        let connected = self.conn.is_some();

        // Start baseline.
        let can_start = connected && self.control.can_start_baseline(now);
        if ui
            .add_enabled(can_start, egui::Button::new("Start baseline"))
            .on_hover_text("Requires: fresh status, SafeIdle, no fault, zero cooldown")
            .clicked()
        {
            let nonce = self.next_nonce();
            let run_id = (host_unix_ns() / 1_000_000_000) as u32;
            let p = self.control.params;
            self.control.on_start_baseline_sent(
                run_id,
                Duration::from_secs(p.baseline_s.into()),
                now,
            );
            self.send(
                Message::StartBaseline(StartBaseline {
                    command_nonce: nonce,
                    run_id,
                    baseline_duration_ms: p.baseline_s * 1000,
                    recovery_duration_ms: p.recovery_s * 1000,
                    requested_sample_hz: p.sample_hz,
                }),
                format!(
                    "StartBaseline run={run_id} baseline={}s recovery={}s rate={}Hz nonce={nonce}",
                    p.baseline_s, p.recovery_s, p.sample_hz
                ),
            );
        }

        // Arm.
        let can_arm = connected && self.control.can_arm(now);
        let arm_label = match self.control.baseline_remaining(now) {
            Some(remaining) if !remaining.is_zero() => {
                let seconds = remaining.as_secs().saturating_add(1);
                format!("Arm pulse ({seconds} s)")
            }
            _ => "Arm pulse".to_owned(),
        };
        if ui
            .add_enabled(can_arm, egui::Button::new(arm_label))
            .on_hover_text(
                "Requires: our run in Baseline/Armed, RTC valid, PGOOD, \
                 recent valid frames, zero cooldown. Never retried automatically.",
            )
            .clicked()
        {
            if let Some(run_id) = self.control.active_run {
                let nonce = self.next_nonce();
                let pulse_ms = self.control.pulse_ms;
                self.control.on_arm_sent(nonce, run_id, pulse_ms);
                self.send(
                    Message::ArmPulse(ArmPulse {
                        command_nonce: nonce,
                        run_id,
                        pulse_duration_ms: pulse_ms,
                    }),
                    format!("ArmPulse run={run_id} pulse_ms={pulse_ms} nonce={nonce}"),
                );
            }
        }

        // Fire (opens confirmation modal; the modal does the send).
        let can_fire = connected && self.control.can_fire(now) && self.fire_modal.is_none();
        if ui
            .add_enabled(can_fire, egui::Button::new("Fire pulse…"))
            .on_hover_text("Opens a confirmation dialog. Requires a successful arm ack and an Armed board status.")
            .clicked()
        {
            if let (crate::control::ArmPhase::ArmAcked { run_id, pulse_ms }, nonce) =
                (self.control.phase, self.next_nonce())
            {
                self.fire_modal = Some(FireModal {
                    run_id,
                    pulse_ms,
                    nonce,
                    opened: now,
                });
            }
        }

        // Abort: prominent whenever a run is active.
        if self.control.abort_relevant() {
            ui.add_space(4.0);
            let abort =
                egui::Button::new(RichText::new("ABORT RUN").color(Color32::WHITE).strong())
                    .fill(Color32::from_rgb(190, 40, 30));
            if ui
                .add_enabled(connected, abort)
                .on_hover_text(
                    "Stops the run at the earliest safe opportunity. Cannot shorten a \
                     hardware pulse already in progress.",
                )
                .clicked()
            {
                let run_id = self
                    .control
                    .last_status()
                    .map(|s| s.run_id)
                    .or(self.control.active_run)
                    .unwrap_or(0);
                let nonce = self.next_nonce();
                self.control.on_abort_sent();
                self.send(
                    Message::Abort(Abort {
                        command_nonce: nonce,
                        run_id,
                    }),
                    format!("Abort run={run_id} nonce={nonce}"),
                );
            }
        }

        if connected {
            ui.add_space(4.0);
            if ui
                .button("Query board")
                .on_hover_text("Requests Hello + Status (read-only)")
                .clicked()
            {
                let nonce = self.next_nonce();
                self.send(
                    Message::Query(Query {
                        command_nonce: nonce,
                    }),
                    format!("Query nonce={nonce}"),
                );
            }
        }
        if self.queue_depth > 0 {
            ui.label(
                RichText::new(format!(
                    "⏳ {} command(s) queued for next command window",
                    self.queue_depth
                ))
                .color(Color32::from_rgb(200, 120, 0)),
            )
            .on_hover_text(
                "The RS-485 bus is half-duplex: commands transmit only \
                 during a board-marked command window and are never retried.",
            );
        }
        if let Some(cmd) = &self.last_command {
            ui.label(RichText::new(format!("last command: {cmd}")).weak().small());
        }
    }

    fn fire_confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = &self.fire_modal else {
            return;
        };
        let run_id = modal.run_id;
        let pulse_ms = modal.pulse_ms;
        let nonce = modal.nonce;
        let opened = modal.opened;
        let mut close = false;

        egui::Modal::new(egui::Id::new("fire_confirm")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("Fire heater pulse?").heading());
            ui.add_space(6.0);
            egui::Grid::new("fire_grid").num_columns(2).show(ui, |ui| {
                ui.label("run id");
                ui.label(RichText::new(format!("{run_id}")).monospace().strong());
                ui.end_row();
                ui.label("pulse duration");
                ui.label(
                    RichText::new(format!(
                        "{:.1} s ({pulse_ms} ms)",
                        f64::from(pulse_ms) / 1000.0
                    ))
                    .strong(),
                );
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "This energizes the 24 V heater in the probe. The command is sent once \
                     and never retried.",
                )
                .small(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                // Deliberately no keyboard shortcut and a short dwell so a
                // double-click on "Fire pulse…" can't click through.
                let dwell_ok = Instant::now().duration_since(opened) > Duration::from_millis(400);
                let now = Instant::now();
                let still_ok = self.conn.is_some() && self.control.can_fire(now);
                let fire_btn = egui::Button::new(
                    RichText::new(format!("FIRE {pulse_ms} ms")).color(Color32::WHITE),
                )
                .fill(Color32::from_rgb(190, 40, 30));
                // Pointer clicks only: no keyboard shortcut, and a focused
                // button must not fire via Enter/Space.
                let pointer_click = ui.input(|i| i.pointer.any_click());
                if ui.add_enabled(dwell_ok && still_ok, fire_btn).clicked() && pointer_click {
                    // One-use nonce: if this refuses, nothing is sent.
                    if self.control.on_fire_sent(nonce, run_id) {
                        self.send(
                            Message::FirePulse(FirePulse {
                                command_nonce: nonce,
                                run_id,
                            }),
                            format!("FirePulse run={run_id} pulse_ms={pulse_ms} nonce={nonce}"),
                        );
                    }
                    close = true;
                }
                if !still_ok {
                    ui.label(
                        RichText::new("board no longer armed")
                            .color(Color32::RED)
                            .small(),
                    );
                }
            });
        });
        if close {
            self.fire_modal = None;
        }
    }

    fn plots(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("history").weak());
            for (secs, label) in WINDOW_CHOICES {
                ui.selectable_value(&mut self.window_secs, secs, label);
            }
            ui.separator();
            let simulated = self
                .conn
                .as_ref()
                .is_some_and(|connection| connection.simulated);
            for (i, info) in CHANNELS.iter().enumerate() {
                let known_broken = info.known_broken && !simulated;
                let mut text = RichText::new(info.id).color(info.color);
                if known_broken {
                    text = text.strikethrough();
                }
                ui.toggle_value(&mut self.show_channel[i], text)
                    .on_hover_text(format!(
                        "{} — {} wire{}",
                        info.position,
                        info.wire_color,
                        if known_broken {
                            " (known broken on current probe)"
                        } else {
                            ""
                        }
                    ));
            }
            ui.separator();
            ui.toggle_value(&mut self.show_raw_plot, "raw counts");
        });

        let t_max = self.board.latest.as_ref().map(|p| p.t_rel_s).unwrap_or(0.0);
        let t_min = t_max - self.window_secs;

        let plot_h = if self.show_raw_plot {
            (ui.available_height() - 8.0) / 2.0
        } else {
            ui.available_height()
        };

        egui_plot::Plot::new("temp_plot")
            .height(plot_h.max(120.0))
            .legend(egui_plot::Legend::default().position(egui_plot::Corner::LeftTop))
            .y_axis_label("°C")
            .x_axis_label("s since connect")
            .show(ui, |plot_ui| {
                for (i, info) in CHANNELS.iter().enumerate() {
                    if !self.show_channel[i] {
                        continue;
                    }
                    let pts = self.temp_series[i].window(t_min);
                    if pts.is_empty() {
                        continue;
                    }
                    plot_ui.line(
                        egui_plot::Line::new(
                            format!("{} {}", info.id, info.position),
                            egui_plot::PlotPoints::from(pts),
                        )
                        .color(info.color),
                    );
                }
            });

        if self.show_raw_plot {
            ui.add_space(4.0);
            egui_plot::Plot::new("raw_plot")
                .height((ui.available_height() - 4.0).max(120.0))
                .legend(egui_plot::Legend::default().position(egui_plot::Corner::LeftTop))
                .y_axis_label("ADC counts")
                .x_axis_label("s since connect")
                .show(ui, |plot_ui| {
                    for (i, info) in CHANNELS.iter().enumerate() {
                        if !self.show_channel[i] {
                            continue;
                        }
                        let pts = self.raw_series[i].window(t_min);
                        if pts.is_empty() {
                            continue;
                        }
                        plot_ui.line(
                            egui_plot::Line::new(
                                format!("{} raw", info.id),
                                egui_plot::PlotPoints::from(pts),
                            )
                            .color(info.color),
                        );
                    }
                    let pts = self.exc_series.window(t_min);
                    if !pts.is_empty() {
                        plot_ui.line(
                            egui_plot::Line::new("excitation", egui_plot::PlotPoints::from(pts))
                                .color(EXCITATION_COLOR),
                        );
                    }
                });
        }
    }
}

impl eframe::App for MonitorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        let ctx = ui.ctx().clone();

        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.add_space(2.0);
            self.top_bar(&ctx, ui);
            ui.add_space(2.0);
        });

        egui::Panel::right("side_panel")
            .resizable(true)
            .default_size(340.0)
            .show(ui, |ui| {
                self.side_panel(ui);
            });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.plots(ui);
        });

        self.fire_confirm_modal(&ctx);

        // Steady low-rate repaint so countdowns/staleness update even when
        // no events arrive; live data also wakes us via the worker.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn on_exit(&mut self) {
        self.disconnect();
    }
}

fn state_color(state: BoardState) -> Color32 {
    match state {
        BoardState::SafeIdle => Color32::from_rgb(110, 110, 110),
        BoardState::Baseline => Color32::from_rgb(30, 100, 220),
        BoardState::Armed => Color32::from_rgb(200, 120, 0),
        BoardState::Pulsing => Color32::from_rgb(200, 30, 30),
        BoardState::Recovery => Color32::from_rgb(0, 140, 140),
        BoardState::Cooldown => Color32::from_rgb(150, 80, 200),
        BoardState::Fault => Color32::from_rgb(190, 0, 0),
    }
}

fn bool_badge(ui: &mut Ui, ok: bool, ok_text: &str, bad_text: &str) {
    if ok {
        ui.label(RichText::new(ok_text).color(Color32::from_rgb(0, 140, 60)));
    } else {
        ui.label(RichText::new(bad_text).color(Color32::RED).strong());
    }
}

fn format_rtc_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S%.3f %Z")
                .to_string()
        })
        .unwrap_or_else(|| format!("{ms} ms (out of range)"))
}

#[cfg(test)]
mod tests {
    use super::format_rtc_ms;

    #[test]
    fn rtc_display_formats_milliseconds_without_panicking() {
        let text = format_rtc_ms(1_772_000_123_456);
        assert!(text.contains(".456"));
    }

    #[test]
    fn rtc_display_handles_out_of_range_values() {
        assert_eq!(
            format_rtc_ms(i64::MAX),
            format!("{} ms (out of range)", i64::MAX)
        );
    }
}
