//! Connection worker.
//!
//! One background thread owns the byte source (serial port or simulator)
//! for the lifetime of a connection. All disk logging happens here, on the
//! receive path, *before* events are offered to the UI over a bounded
//! channel — so a slow or stalled UI can drop display events (counted) but
//! can never lose logged data.

use crate::logger::{Logger, LoggerConfig};
use crate::sim::SimBoard;
use crate::stats::{RateWindow, SeqTracker, StatsSnapshot};
use crate::types::SampleData;
use crate::types::{host_unix_ns, state_name, ProcessedSample};
use sap_flux_protocol::{
    encode, Ack, Decoder, ErrorReport, FieldStatus, Frame, Hello, Message, Query, SetRtc, Status,
    FLAG_COMMAND_WINDOW, MAX_ENCODED_LEN,
};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// UI-bound event channel depth. Sized for several seconds of 50 Hz
/// samples; overflow drops display events (counted), never logged data.
const UI_CHANNEL_DEPTH: usize = 8192;
/// How often stats snapshots are pushed to the UI.
const STATS_INTERVAL: Duration = Duration::from_millis(250);
/// FT232R driver-enable settle time: RTS is asserted this long before the
/// first transmitted byte.
const RTS_SETTLE: Duration = Duration::from_micros(200);
/// No frame for this long means the board is not streaming (it streams a
/// frame every 40 ms while a host is attached), so the bus is free.
pub const BOARD_SILENT: Duration = Duration::from_secs(2);
/// Wake attempts on a silent bus repeat this often.
pub const WAKE_RETRY: Duration = Duration::from_secs(5);
/// After the wake byte, the board needs this long out of Stop before it
/// decodes a command (it waits 1 ms, then drops the garbled wake byte).
const WAKE_DELAY: Duration = Duration::from_millis(15);
/// The board stops streaming 10 min after the last host frame; a `Query`
/// this often keeps it attached while the monitor is connected.
pub const KEEPALIVE: Duration = Duration::from_secs(60);

/// What to connect to.
#[derive(Debug, Clone)]
pub enum ConnectionTarget {
    /// A real serial port, 8N1.
    Serial {
        /// OS port path, e.g. `/dev/cu.usbserial-BG02Q2RS`.
        port: String,
        /// Baud rate (default 115200).
        baud: u32,
    },
    /// The built-in simulator (never opens a port).
    Simulator {
        /// Inject periodic CRC errors and sequence gaps.
        inject_errors: bool,
    },
}

impl ConnectionTarget {
    /// Display name used in the UI and metadata.
    pub fn name(&self) -> String {
        match self {
            ConnectionTarget::Serial { port, .. } => port.clone(),
            ConnectionTarget::Simulator { .. } => "simulator".into(),
        }
    }
}

/// Commands from the UI to the worker.
pub enum WorkerCmd {
    /// Encode and transmit a message (explicit user action only).
    ///
    /// On a real serial link the encoded frame is *queued*, never written
    /// immediately: the RS-485 bus is half-duplex, so the worker transmits
    /// at most one queued command per command window (a Sample carrying
    /// [`FLAG_COMMAND_WINDOW`], after which the board stays silent and
    /// listening). The simulator consumes commands immediately.
    Send {
        /// The message to send.
        message: Message,
        /// Short human description for the event log.
        desc: String,
    },
    /// Stop the worker, finalize the session.
    Shutdown,
}

/// One typed host command waiting for a transmit window.
pub struct QueuedCommand {
    /// Typed message. Encoding is deferred until physical transmission so
    /// time-sensitive fields can be refreshed at the command window.
    pub message: Message,
    /// Short human description for the event log.
    pub desc: String,
}

/// Refresh fields whose meaning is tied to physical transmission time.
///
/// This is deliberately pure so the queued-command behavior can be tested
/// without sleeping or reading the wall clock.
pub fn prepare_message_for_send(message: Message, unix_ms: i64) -> Message {
    match message {
        Message::SetRtc(set_rtc) => Message::SetRtc(SetRtc {
            command_nonce: set_rtc.command_nonce,
            unix_ms,
        }),
        other => other,
    }
}

fn sent_description(message: &Message, queued_desc: String) -> String {
    match message {
        Message::SetRtc(set_rtc) => format!(
            "SetRtc unix_ms={} nonce={}",
            set_rtc.unix_ms, set_rtc.command_nonce
        ),
        _ => queued_desc,
    }
}

/// FIFO of queued host commands for a real serial link.
///
/// A queued command is one intended transmission: it is popped exactly once
/// at a command window, is never re-queued, and is never retried on a
/// missing ack or timeout (this is absolute for FirePulse). Disconnect
/// drops every unsent command.
#[derive(Default)]
pub struct CommandQueue {
    queue: VecDeque<QueuedCommand>,
}

impl CommandQueue {
    /// Append a command; returns the new queue depth.
    pub fn push(&mut self, cmd: QueuedCommand) -> usize {
        self.queue.push_back(cmd);
        self.queue.len()
    }

    /// Pop the oldest command, but only when `window` is true (a Sample
    /// carrying [`FLAG_COMMAND_WINDOW`] was just decoded). At most one
    /// command leaves the queue per window; outside a window nothing does.
    pub fn pop_for_window(&mut self, window: bool) -> Option<QueuedCommand> {
        if window {
            self.queue.pop_front()
        } else {
            None
        }
    }

    /// Drop every unsent command (disconnect); returns how many were lost.
    pub fn clear(&mut self) -> usize {
        let n = self.queue.len();
        self.queue.clear();
        n
    }

    /// Number of commands waiting for a window.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// True when nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// Traffic the worker originates on a real serial link to keep the board
/// streaming (`firmware/sap-flux-blinky/README.md`, link selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAction {
    /// The bus is silent: send the `0x00` wake byte, wait, then a `Query`
    /// directly (no command window exists while the board is quiet).
    /// `first` is set for the first attempt of a silent stretch.
    Wake {
        /// First attempt since the board went quiet.
        first: bool,
    },
    /// The board streams: queue a `Query` for the next command window.
    Keepalive,
}

/// Decides when to wake the board or keep it attached. Pure, so tests can
/// drive it with synthetic time.
pub struct LinkKeeper {
    connected_at: Instant,
    last_wake: Option<Instant>,
    last_tx: Instant,
}

impl LinkKeeper {
    /// New keeper for a link opened at `now`. The first wake waits
    /// [`BOARD_SILENT`], so it never collides with a board already streaming.
    pub fn new(now: Instant) -> Self {
        Self {
            connected_at: now,
            last_wake: None,
            last_tx: now,
        }
    }

    /// A command was transmitted (any command refreshes the board's attach).
    pub fn on_tx(&mut self, now: Instant) {
        self.last_tx = now;
    }

    /// What to send now, given when the last frame was decoded.
    pub fn poll(&mut self, now: Instant, last_frame: Option<Instant>) -> Option<KeepAction> {
        let heard = last_frame
            .unwrap_or(self.connected_at)
            .max(self.connected_at);
        if now.duration_since(heard) >= BOARD_SILENT {
            let first = self.last_wake.is_none_or(|t| t < heard);
            if first
                || self
                    .last_wake
                    .is_some_and(|t| now.duration_since(t) >= WAKE_RETRY)
            {
                self.last_wake = Some(now);
                self.last_tx = now;
                return Some(KeepAction::Wake { first });
            }
            return None;
        }
        if now.duration_since(self.last_tx) >= KEEPALIVE {
            self.last_tx = now;
            return Some(KeepAction::Keepalive);
        }
        None
    }
}

/// Events from the worker to the UI.
pub enum UiEvent {
    /// Connection established (worker started successfully).
    Connected {
        /// Port name or "simulator".
        port: String,
        /// True in simulator mode.
        simulated: bool,
    },
    /// Worker stopped. `reason` is `None` for a user-requested disconnect.
    Disconnected {
        /// Failure description, if the link died on its own.
        reason: Option<String>,
    },
    /// Board identification received.
    HelloMsg(Hello),
    /// Status frame received.
    StatusMsg(Status),
    /// Ack frame received.
    AckMsg(Ack),
    /// Error report frame received.
    ErrorMsg(ErrorReport),
    /// Field-mode status frame received.
    FieldStatusMsg(FieldStatus),
    /// Decoded and converted sample.
    SampleMsg(Box<ProcessedSample>),
    /// A frame failed to decode (already counted and logged).
    DecodeError(String),
    /// A capture session directory was created.
    SessionStarted(PathBuf),
    /// A logging I/O failure occurred (shown prominently in the UI).
    LogError(String),
    /// Periodic counters snapshot.
    Stats(StatsSnapshot),
    /// A command was physically transmitted (never emitted for a command
    /// that is merely queued).
    CommandSent(String),
    /// A command was queued for the next command window (real serial only).
    CommandQueued {
        /// Short human description of the queued command.
        desc: String,
        /// Queue depth after enqueueing.
        depth: usize,
    },
}

/// Handle owned by the UI for one worker/connection.
pub struct WorkerHandle {
    /// Send commands to the worker.
    pub cmd_tx: std::sync::mpsc::Sender<WorkerCmd>,
    /// Receive events from the worker.
    pub ui_rx: Receiver<UiEvent>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Request shutdown and wait for the worker to finish (flushes and
    /// finalizes the capture session). Keeps draining the event channel
    /// while waiting so the worker can never deadlock on a full channel.
    pub fn shutdown(&mut self) {
        let _ = self.cmd_tx.send(WorkerCmd::Shutdown);
        if let Some(j) = self.join.take() {
            while !j.is_finished() {
                while self.ui_rx.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = j.join();
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The receive-path state shared by real and simulated connections:
/// decode, account, convert, log, then emit UI events. Separated from the
/// I/O loop so tests can drive it with synthetic bytes.
pub struct Pipeline {
    decoder: Decoder,
    seq: SeqTracker,
    /// Session logger (public so tests can inspect it).
    pub logger: Logger,
    rate: RateWindow,
    stats: StatsSnapshot,
    start: Instant,
    last_log_error: Option<String>,
    command_window: bool,
    last_frame_at: Option<Instant>,
}

impl Pipeline {
    /// New pipeline; `start` anchors the plot time axis.
    pub fn new(logger: Logger, start: Instant) -> Self {
        Self {
            decoder: Decoder::new(),
            seq: SeqTracker::default(),
            logger,
            rate: RateWindow::new(Duration::from_secs(2)),
            stats: StatsSnapshot::default(),
            start,
            last_log_error: None,
            command_window: false,
            last_frame_at: None,
        }
    }

    /// When the last frame was decoded (`None` before the first).
    pub fn last_frame_at(&self) -> Option<Instant> {
        self.last_frame_at
    }

    /// True when the most recently decoded frame was a Sample carrying
    /// [`FLAG_COMMAND_WINDOW`] — i.e. the board is now silent and listening.
    /// Any later frame closes the window, and reading clears the flag, so
    /// each window authorizes at most one transmission.
    pub fn take_command_window(&mut self) -> bool {
        std::mem::take(&mut self.command_window)
    }

    /// Current counters (rate evaluated at `now`).
    pub fn snapshot(&mut self, now: Instant) -> StatsSnapshot {
        self.stats.sample_rate_hz = self.rate.rate(now);
        self.stats.seq_gaps = self.seq.gaps;
        self.stats.seq_missing = self.seq.missing;
        self.stats
    }

    /// Count a UI event dropped due to backpressure.
    pub fn count_ui_drop(&mut self) {
        self.stats.ui_events_dropped += 1;
    }

    /// Count transmitted bytes.
    pub fn count_tx(&mut self, n: usize) {
        self.stats.bytes_tx += n as u64;
    }

    /// Feed received bytes; pushes resulting UI events into `out`.
    pub fn ingest(&mut self, bytes: &[u8], now: Instant, out: &mut Vec<UiEvent>) {
        self.stats.bytes_rx += bytes.len() as u64;
        self.logger.on_raw(bytes);
        for &b in bytes {
            match self.decoder.push(b) {
                None => {}
                Some(Ok(frame)) => self.handle_frame(frame, now, out),
                Some(Err(e)) => {
                    // Garbage after a window sample also means the bus is
                    // no longer quiet: the window has passed.
                    self.command_window = false;
                    if matches!(e, sap_flux_protocol::DecodeError::Crc { .. }) {
                        self.stats.crc_errors += 1;
                    } else {
                        self.stats.framing_errors += 1;
                    }
                    let desc = format!("{e:?}");
                    self.logger.log_event(host_unix_ns(), "parser_error", &desc);
                    out.push(UiEvent::DecodeError(desc));
                }
            }
        }
        self.surface_log_errors(out);
    }

    fn handle_frame(&mut self, frame: Frame, now: Instant, out: &mut Vec<UiEvent>) {
        let ns = host_unix_ns();
        self.stats.frames_ok += 1;
        self.last_frame_at = Some(now);
        // A window sample opens the command window; any frame after it means
        // the board is transmitting again, so the window has passed.
        self.command_window = match frame.message {
            Message::Sample(s) => s.flags & FLAG_COMMAND_WINDOW != 0,
            Message::Sample12(s) => s.flags & FLAG_COMMAND_WINDOW != 0,
            _ => false,
        };
        if let Some(missing) = self.seq.on_sequence(frame.sequence) {
            self.logger.log_event(
                ns,
                "seq_gap",
                &format!("{missing} frame(s) missing before seq {}", frame.sequence),
            );
        }
        // A capture session begins on the first valid Hello or Sample.
        if matches!(
            frame.message,
            Message::Hello(_) | Message::Sample(_) | Message::Sample12(_) | Message::FieldStatus(_)
        ) {
            if let Some(dir) = self.logger.ensure_session(ns, now) {
                out.push(UiEvent::SessionStarted(dir));
            }
        }
        match frame.message {
            Message::Hello(h) => {
                self.logger.set_board_info(&h);
                self.logger.log_event(
                    ns,
                    "hello",
                    &format!(
                        "board_id={:016X} fw={} caps={:#x}",
                        h.board_id,
                        crate::types::format_fw_version(h.firmware_version),
                        h.capabilities
                    ),
                );
                out.push(UiEvent::HelloMsg(h));
            }
            Message::Sample(_) | Message::Sample12(_) => {
                let data: SampleData = match frame.message {
                    Message::Sample(s) => s.into(),
                    Message::Sample12(s) => s.into(),
                    _ => unreachable!(),
                };
                let ps = ProcessedSample::build(
                    data,
                    frame.sequence,
                    ns,
                    now.duration_since(self.start).as_secs_f64(),
                );
                self.stats.samples_ok += 1;
                self.rate.tick(now);
                if !ps.sample_valid {
                    self.stats.invalid_samples += 1;
                }
                self.logger.log_sample(&ps);
                out.push(UiEvent::SampleMsg(Box::new(ps)));
            }
            Message::Status(st) => {
                self.logger.log_event(
                    ns,
                    "status",
                    &format!(
                        "state={} run={} fault={} pulse_ms={} cooldown_ms={} rtc_ms={}",
                        state_name(st.state),
                        st.run_id,
                        st.fault_code,
                        st.pulse_duration_ms,
                        st.cooldown_remaining_ms,
                        st.rtc_unix_ms
                    ),
                );
                // Status frames mark state transitions: flush immediately.
                self.logger.force_flush();
                out.push(UiEvent::StatusMsg(st));
            }
            Message::Ack(a) => {
                self.logger.log_event(
                    ns,
                    "ack",
                    &format!(
                        "command_type={:#04x} result={} nonce={} run={}",
                        a.command_type, a.result, a.command_nonce, a.run_id
                    ),
                );
                self.logger.force_flush();
                out.push(UiEvent::AckMsg(a));
            }
            Message::Error(e) => {
                self.logger.log_event(
                    ns,
                    "error_report",
                    &format!("fault_code={:#06x} detail={:#06x}", e.fault_code, e.detail),
                );
                self.logger.force_flush();
                out.push(UiEvent::ErrorMsg(e));
            }
            Message::FieldStatus(fs) => {
                self.logger
                    .log_event(ns, "field_status", &crate::types::field_status_detail(&fs));
                self.logger.force_flush();
                out.push(UiEvent::FieldStatusMsg(fs));
            }
            Message::WindowInfo(w) => {
                self.logger.log_event(
                    ns,
                    "window_info",
                    &format!(
                        "run={} flags={:#06x} channels={} bin_ms={} n_bins={} chunks={} \
                         start_rtc_ms={} heater_on_offset_ms={} pulse_ms={} sample_hz={} \
                         frames={}/{} missing data_crc32={:08x}",
                        w.run_id,
                        w.window_flags,
                        w.channels,
                        w.bin_ms(),
                        w.n_bins,
                        w.chunk_count,
                        w.start_rtc_unix_ms,
                        w.heater_on_offset_ms,
                        w.pulse_duration_ms,
                        w.sample_hz,
                        w.frames_missing,
                        w.frames_total,
                        w.data_crc32
                    ),
                );
            }
            Message::WindowChunk(c) => {
                self.logger.log_event(
                    ns,
                    "window_chunk",
                    &format!("run={} chunk={} len={}", c.run_id, c.chunk, c.len),
                );
            }
            // Host-to-board messages echoed back would be a wiring anomaly;
            // record them but nothing else.
            other => {
                self.logger
                    .log_event(ns, "unexpected_rx", &format!("{other:?}"));
            }
        }
    }

    /// Record a physically transmitted command in the event log. Must only
    /// be called when bytes actually left the port, never for a queued or
    /// failed command.
    pub fn log_command(&mut self, desc: &str, out: &mut Vec<UiEvent>) {
        self.logger.log_event(host_unix_ns(), "command_tx", desc);
        self.logger.force_flush();
        out.push(UiEvent::CommandSent(desc.to_string()));
        self.surface_log_errors(out);
    }

    /// Record that a command was queued for the next command window.
    pub fn log_command_queued(&mut self, desc: &str, depth: usize, out: &mut Vec<UiEvent>) {
        self.logger
            .log_event(host_unix_ns(), "command_queued", desc);
        self.logger.force_flush();
        out.push(UiEvent::CommandQueued {
            desc: desc.to_string(),
            depth,
        });
        self.surface_log_errors(out);
    }

    /// Note a connection-lifecycle event.
    pub fn log_lifecycle(&mut self, kind: &str, detail: &str) {
        self.logger.log_event(host_unix_ns(), kind, detail);
    }

    /// Finalize the capture session with final counters.
    pub fn finalize(&mut self, now: Instant) {
        let snap = self.snapshot(now);
        self.logger.finalize(host_unix_ns(), &snap);
    }

    fn surface_log_errors(&mut self, out: &mut Vec<UiEvent>) {
        if self.logger.last_error != self.last_log_error {
            if let Some(e) = &self.logger.last_error {
                out.push(UiEvent::LogError(e.clone()));
            }
            self.last_log_error = self.logger.last_error.clone();
        }
    }
}

enum Source {
    Serial(Box<dyn serialport::SerialPort>),
    Sim(SimBoard),
}

/// Transmit one already-encoded frame inside a command window on an
/// FT232R-style adapter whose RS-485 driver is enabled by RTS HIGH:
/// assert RTS, allow the driver to enable, write + flush, and return to
/// receive mode — dropping RTS best-effort even when the write fails.
fn transmit_in_window(port: &mut dyn serialport::SerialPort, bytes: &[u8]) -> std::io::Result<()> {
    port.write_request_to_send(true)?;
    std::thread::sleep(RTS_SETTLE);
    let write = port.write_all(bytes).and_then(|_| port.flush());
    // Back to receive mode immediately, even after a failed write:
    // leaving RTS high jams the half-duplex bus.
    let rts = port.write_request_to_send(false);
    write?;
    rts.map_err(std::io::Error::from)
}

/// Spawn a worker for `target`. Events flow to the returned handle;
/// `egui_ctx` is woken whenever new events are queued.
pub fn spawn(
    target: ConnectionTarget,
    captures_root: PathBuf,
    egui_ctx: Option<eframe::egui::Context>,
) -> WorkerHandle {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<WorkerCmd>();
    let (ui_tx, ui_rx) = sync_channel::<UiEvent>(UI_CHANNEL_DEPTH);
    let join = std::thread::Builder::new()
        .name("sapflux-worker".into())
        .spawn(move || run_worker(target, captures_root, ui_tx, cmd_rx, egui_ctx))
        .expect("spawn worker thread");
    WorkerHandle {
        cmd_tx,
        ui_rx,
        join: Some(join),
    }
}

fn run_worker(
    target: ConnectionTarget,
    captures_root: PathBuf,
    ui_tx: SyncSender<UiEvent>,
    cmd_rx: std::sync::mpsc::Receiver<WorkerCmd>,
    egui_ctx: Option<eframe::egui::Context>,
) {
    let start = Instant::now();
    let mut rts_error: Option<String> = None;
    let (mut source, baud, simulated) = match &target {
        ConnectionTarget::Serial { port, baud } => {
            match serialport::new(port, *baud)
                .data_bits(serialport::DataBits::Eight)
                .parity(serialport::Parity::None)
                .stop_bits(serialport::StopBits::One)
                .flow_control(serialport::FlowControl::None)
                .timeout(Duration::from_millis(20))
                .open()
            {
                Ok(mut p) => {
                    // FT232R: RTS HIGH enables the RS-485 transmitter and
                    // jams the half-duplex bus. Enter receive mode first.
                    if let Err(e) = p.write_request_to_send(false) {
                        rts_error = Some(format!(
                            "failed to set RTS low (receive mode) on {port}: {e}"
                        ));
                    }
                    (Source::Serial(p), *baud, false)
                }
                Err(e) => {
                    let _ = ui_tx.send(UiEvent::Disconnected {
                        reason: Some(format!("open {port}: {e}")),
                    });
                    if let Some(ctx) = &egui_ctx {
                        ctx.request_repaint();
                    }
                    return;
                }
            }
        }
        ConnectionTarget::Simulator { inject_errors } => {
            (Source::Sim(SimBoard::new(start, *inject_errors)), 0, true)
        }
    };

    let logger = Logger::new(LoggerConfig {
        captures_root,
        port_name: target.name(),
        baud,
        simulated,
    });
    let mut pipeline = Pipeline::new(logger, start);
    pipeline.log_lifecycle(
        "connect",
        &format!("target={} baud={baud} simulated={simulated}", target.name()),
    );

    let _ = ui_tx.send(UiEvent::Connected {
        port: target.name(),
        simulated,
    });
    if let Some(ctx) = &egui_ctx {
        ctx.request_repaint();
    }

    let mut events: Vec<UiEvent> = Vec::new();
    if let Some(e) = rts_error {
        // The bus may be jammed with the transmitter enabled; surface this
        // prominently so the user can power-cycle the adapter.
        pipeline.log_lifecycle("rts_error", &e);
        events.push(UiEvent::LogError(e));
    }

    let mut read_buf = [0u8; 4096];
    let mut sim_buf: Vec<u8> = Vec::new();
    let mut cmd_queue = CommandQueue::default();
    let mut keeper = LinkKeeper::new(Instant::now());
    let mut keeper_nonce = host_unix_ns() as u64;
    let mut tx_seq: u32 = 0;
    let mut last_stats = Instant::now();
    let mut exit_reason: Option<String> = None;
    let mut user_requested_exit = false;

    'main: loop {
        // 1) Drain commands (explicit user actions; the worker originates
        //    only the wake and keepalive of step 2c). On a real serial link
        //    nothing is written here: the bus is half-duplex, so commands
        //    queue for the next command window. The simulator consumes
        //    immediately.
        loop {
            match cmd_rx.try_recv() {
                Ok(WorkerCmd::Send { message, desc }) => match &mut source {
                    Source::Serial(_) => {
                        let depth = cmd_queue.push(QueuedCommand {
                            message,
                            desc: desc.clone(),
                        });
                        pipeline.log_command_queued(&desc, depth, &mut events);
                    }
                    Source::Sim(sim) => {
                        let message = prepare_message_for_send(message, host_unix_ns() / 1_000_000);
                        let desc = sent_description(&message, desc);
                        let mut buf = [0u8; MAX_ENCODED_LEN];
                        let n = match encode(&message, tx_seq, &mut buf) {
                            Ok(n) => n,
                            Err(e) => {
                                events.push(UiEvent::LogError(format!("encode failed: {e:?}")));
                                continue;
                            }
                        };
                        tx_seq = tx_seq.wrapping_add(1);
                        pipeline.count_tx(n);
                        sim.handle_command(&message, Instant::now(), &mut sim_buf);
                        pipeline.log_command(&desc, &mut events);
                    }
                },
                Ok(WorkerCmd::Shutdown) => {
                    user_requested_exit = true;
                    break 'main;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    user_requested_exit = true;
                    break 'main;
                }
            }
        }

        // 2) Receive bytes.
        let now = Instant::now();
        match &mut source {
            Source::Serial(port) => match port.read(&mut read_buf) {
                Ok(0) => {}
                Ok(n) => pipeline.ingest(&read_buf[..n], now, &mut events),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    exit_reason = Some(format!("serial read: {e}"));
                    break 'main;
                }
            },
            Source::Sim(sim) => {
                sim.poll(now, &mut sim_buf);
                if !sim_buf.is_empty() {
                    pipeline.ingest(&sim_buf, now, &mut events);
                    sim_buf.clear();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        // 2b) If the stream just ended on a command-window sample, the
        //     board is silent and listening for ~40 ms: transmit at most
        //     one queued command, right now. Queued commands leave only
        //     here; 2c writes only its own wake and Query.
        let window = pipeline.take_command_window();
        if let Source::Serial(port) = &mut source {
            if let Some(cmd) = cmd_queue.pop_for_window(window) {
                let message = prepare_message_for_send(cmd.message, host_unix_ns() / 1_000_000);
                let desc = sent_description(&message, cmd.desc);
                let mut buf = [0u8; MAX_ENCODED_LEN];
                let n = match encode(&message, tx_seq, &mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        events.push(UiEvent::LogError(format!("encode failed: {e:?}")));
                        continue;
                    }
                };
                tx_seq = tx_seq.wrapping_add(1);
                match transmit_in_window(port.as_mut(), &buf[..n]) {
                    Ok(()) => {
                        keeper.on_tx(Instant::now());
                        pipeline.count_tx(n);
                        pipeline.log_command(&desc, &mut events);
                    }
                    Err(e) => {
                        // The command is consumed either way — nothing is
                        // ever retried — and the link is treated as dead.
                        exit_reason = Some(format!("serial write: {e}"));
                        pipeline.log_lifecycle("command_tx_failed", &format!("{desc} ({e})"));
                        events.push(UiEvent::LogError(format!(
                            "command write failed: {} ({e})",
                            desc
                        )));
                        break 'main;
                    }
                }
            }
        }

        // 2c) Keep the board streaming. A silent bus means it stopped
        //     streaming (no host frame for 10 min) and may be in Stop: send
        //     the wake byte and, once it is awake, a Query directly. While
        //     it streams, queue a keepalive Query for a command window.
        if let Source::Serial(port) = &mut source {
            match keeper.poll(Instant::now(), pipeline.last_frame_at()) {
                Some(KeepAction::Wake { first }) => {
                    keeper_nonce = keeper_nonce.wrapping_add(1);
                    let message = Message::Query(Query {
                        command_nonce: keeper_nonce,
                    });
                    let mut buf = [0u8; MAX_ENCODED_LEN];
                    let n = encode(&message, tx_seq, &mut buf).expect("Query encodes");
                    tx_seq = tx_seq.wrapping_add(1);
                    let sent = transmit_in_window(port.as_mut(), &[0x00]).and_then(|()| {
                        std::thread::sleep(WAKE_DELAY);
                        transmit_in_window(port.as_mut(), &buf[..n])
                    });
                    let desc = format!("wake + Query nonce={keeper_nonce}");
                    match sent {
                        Ok(()) => {
                            pipeline.count_tx(n + 1);
                            if first {
                                pipeline.log_command(&desc, &mut events);
                            } else {
                                pipeline.log_lifecycle("wake_retry", &desc);
                            }
                        }
                        Err(e) => {
                            exit_reason = Some(format!("serial write: {e}"));
                            pipeline.log_lifecycle("command_tx_failed", &format!("{desc} ({e})"));
                            events.push(UiEvent::LogError(format!("wake write failed: {e}")));
                            break 'main;
                        }
                    }
                }
                Some(KeepAction::Keepalive) if cmd_queue.is_empty() => {
                    keeper_nonce = keeper_nonce.wrapping_add(1);
                    let desc = format!("Query (keepalive) nonce={keeper_nonce}");
                    let depth = cmd_queue.push(QueuedCommand {
                        message: Message::Query(Query {
                            command_nonce: keeper_nonce,
                        }),
                        desc: desc.clone(),
                    });
                    pipeline.log_command_queued(&desc, depth, &mut events);
                }
                _ => {}
            }
        }

        // 3) Housekeeping: periodic stats + flush.
        let now = Instant::now();
        if now.duration_since(last_stats) >= STATS_INTERVAL {
            last_stats = now;
            events.push(UiEvent::Stats(pipeline.snapshot(now)));
        }
        pipeline.logger.flush_if_due(now);

        // 4) Deliver events to the UI without ever blocking the receive
        //    path: overflow is dropped and counted (data already logged).
        if !events.is_empty() {
            for ev in events.drain(..) {
                if let Err(TrySendError::Full(_)) = ui_tx.try_send(ev) {
                    pipeline.count_ui_drop();
                }
            }
            if let Some(ctx) = &egui_ctx {
                ctx.request_repaint();
            }
        }
    }

    // Shutdown: drop unsent commands, finalize the session, tell the UI.
    if !cmd_queue.is_empty() {
        pipeline.log_lifecycle(
            "queued_commands_dropped",
            &format!(
                "{} unsent command(s) dropped on disconnect",
                cmd_queue.clear()
            ),
        );
    }
    let now = Instant::now();
    pipeline.log_lifecycle(
        "disconnect",
        exit_reason.as_deref().unwrap_or(if user_requested_exit {
            "user disconnect"
        } else {
            "shutdown"
        }),
    );
    pipeline.finalize(now);
    for ev in events.drain(..) {
        let _ = ui_tx.try_send(ev);
    }
    let _ = ui_tx.send(UiEvent::Disconnected {
        reason: exit_reason,
    });
    if let Some(ctx) = &egui_ctx {
        ctx.request_repaint();
    }
}
