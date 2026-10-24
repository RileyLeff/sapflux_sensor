//! End-to-end tests of the receive pipeline: bytes in -> decode -> stats ->
//! disk logging -> UI events. Also drives the simulator through the same
//! pipeline with synthetic time (no sleeping, no serial ports).

use sap_flux_monitor::logger::{Logger, LoggerConfig};
use sap_flux_monitor::sim::SimBoard;
use sap_flux_monitor::worker::{
    prepare_message_for_send, CommandQueue, Pipeline, QueuedCommand, UiEvent,
};
use sap_flux_protocol::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static TEST_ID: AtomicU32 = AtomicU32::new(0);

/// Unique scratch dir for one test; removed on drop.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sapflux_monitor_test_{}_{}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_pipeline(root: &Scratch) -> Pipeline {
    let logger = Logger::new(LoggerConfig {
        captures_root: root.0.clone(),
        port_name: "test".into(),
        baud: 115_200,
        simulated: true,
    });
    Pipeline::new(logger, Instant::now())
}

fn encode_msg(msg: &Message, seq: u32) -> Vec<u8> {
    let mut buf = [0u8; MAX_ENCODED_LEN];
    let n = encode(msg, seq, &mut buf).unwrap();
    buf[..n].to_vec()
}

fn hello() -> Message {
    Message::Hello(Hello {
        board_id: 0xABCD_EF01_2345_6789,
        firmware_version: (1 << 16) | (2 << 8) | 3,
        capabilities: 0,
    })
}

fn sample(seq_hint: u32) -> Message {
    Message::Sample(Sample {
        monotonic_us: 1000 * seq_hint as u64,
        rtc_unix_ms: 1_772_000_000_000 + seq_hint as i64,
        run_id: 1,
        flags: FLAG_RTC_VALID | FLAG_PGOOD | FLAG_EXCITATION_ACTIVE,
        excitation_raw: 26000,
        thermistor_raw: [13000, 12900, 2, 13100, 13050, 12950],
        v24_raw: 24000,
        state: BoardState::Baseline,
    })
}

fn session_dir(events: &[UiEvent]) -> PathBuf {
    events
        .iter()
        .find_map(|e| match e {
            UiEvent::SessionStarted(d) => Some(d.clone()),
            _ => None,
        })
        .expect("session should start")
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap()
}

#[test]
fn session_starts_on_first_sample_and_logs_every_row() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let now = Instant::now();

    // Raw bytes before any valid frame must still end up in raw.cobs.
    let noise = [0x55u8, 0x66, 0x00];
    p.ingest(&noise, now, &mut events);

    let mut stream = Vec::new();
    stream.extend_from_slice(&encode_msg(&hello(), 0));
    for i in 1..=50u32 {
        stream.extend_from_slice(&encode_msg(&sample(i), i));
    }
    p.ingest(&stream, now, &mut events);
    p.finalize(now);

    let dir = session_dir(&events);

    // raw.cobs is byte-exact: pre-session noise + the whole stream.
    let raw = std::fs::read(dir.join("raw.cobs")).unwrap();
    let mut expected = noise.to_vec();
    expected.extend_from_slice(&stream);
    assert_eq!(raw, expected);

    // samples.csv: header + one row per decoded sample, no averaging.
    let samples = read(&dir, "samples.csv");
    let lines: Vec<&str> = samples.lines().collect();
    assert_eq!(lines.len(), 1 + 50);
    let header_cols = lines[0].split(',').count();
    for (i, line) in lines[1..].iter().enumerate() {
        assert_eq!(
            line.split(',').count(),
            header_cols,
            "row {i} has wrong column count"
        );
    }
    // Spot-check a row: T3 (raw 2) must be invalid with empty derived
    // fields; T1 must have a plausible temperature.
    let cols: Vec<&str> = lines[1].split(',').collect();
    let h: Vec<&str> = lines[0].split(',').collect();
    let col = |name: &str| -> &str {
        let idx = h
            .iter()
            .position(|c| *c == name)
            .unwrap_or_else(|| panic!("no column {name}"));
        cols[idx]
    };
    assert_eq!(col("sequence"), "1");
    assert_eq!(col("t3_raw"), "2");
    assert_eq!(col("t3_valid"), "0");
    assert_eq!(col("t3_temp_c"), "");
    assert_eq!(col("t1_valid"), "1");
    let t1: f64 = col("t1_temp_c").parse().unwrap();
    assert!((0.0..50.0).contains(&t1), "t1 = {t1}");
    assert_eq!(col("pgood"), "1");
    assert_eq!(col("state_name"), "Baseline");

    // events.csv has the hello, session_start, and session_end.
    let ev = read(&dir, "events.csv");
    assert!(ev.contains("session_start"));
    assert!(ev.contains("hello"));
    assert!(ev.contains("session_end"));

    // metadata.json is valid JSON with the key fields.
    let meta: serde_json::Value = serde_json::from_str(&read(&dir, "metadata.json")).unwrap();
    assert_eq!(meta["protocol_version"], 1);
    assert_eq!(meta["board_id_hex"], "ABCDEF0123456789");
    assert_eq!(meta["firmware_version"], "1.2.3");
    assert_eq!(meta["channels"][0]["known_broken"], false);
    assert_eq!(meta["channels"][0]["wire_color"], "green");
    assert_eq!(meta["channels"][2]["known_broken"], false);
    assert_eq!(meta["channels"][2]["wire_color"], "purple");
    assert_eq!(meta["final_counters"]["samples_ok"], 50);
    assert_eq!(meta["final_counters"]["sample_rows_written"], 50);
    assert!(meta["session_end_unix_ns"].is_i64());

    let snap = p.snapshot(now);
    assert_eq!(snap.samples_ok, 50);
    assert_eq!(snap.frames_ok, 51);
    assert_eq!(snap.crc_errors, 0);
    assert_eq!(snap.seq_gaps, 0);
}

#[test]
fn corrupt_frames_counted_logged_and_recovered() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let now = Instant::now();

    let mut stream = encode_msg(&hello(), 0);
    // Good sample, then a corrupted one, then a good one (seq continues, so
    // the corrupted frame also registers as a sequence gap).
    stream.extend_from_slice(&encode_msg(&sample(1), 1));
    let mut bad = encode_msg(&sample(2), 2);
    bad[10] ^= 0xFF;
    stream.extend_from_slice(&bad);
    stream.extend_from_slice(&encode_msg(&sample(3), 3));
    p.ingest(&stream, now, &mut events);

    let snap = p.snapshot(now);
    assert_eq!(snap.samples_ok, 2);
    assert_eq!(
        snap.crc_errors + snap.framing_errors,
        1,
        "one corrupt frame -> one error"
    );
    assert_eq!(snap.seq_gaps, 1, "lost frame shows up as a sequence gap");
    assert_eq!(snap.seq_missing, 1);

    p.finalize(now);
    let dir = session_dir(&events);
    let ev = read(&dir, "events.csv");
    assert!(ev.contains("parser_error"));
    assert!(ev.contains("seq_gap"));

    // Both good samples logged; the corrupted one is absent (never decoded).
    let samples = read(&dir, "samples.csv");
    assert_eq!(samples.lines().count(), 1 + 2);
}

#[test]
fn byte_at_a_time_delivery_decodes_identically() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let now = Instant::now();
    let mut stream = encode_msg(&hello(), 7);
    for i in 0..10u32 {
        stream.extend_from_slice(&encode_msg(&sample(i), 8 + i));
    }
    for b in stream {
        p.ingest(&[b], now, &mut events);
    }
    let snap = p.snapshot(now);
    assert_eq!(snap.frames_ok, 11);
    assert_eq!(snap.samples_ok, 10);
    assert_eq!(snap.seq_gaps, 0);
}

#[test]
fn invalid_samples_counted_but_still_logged() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let now = Instant::now();

    // ADC-error sample: logged, counted invalid.
    let mut s = match sample(1) {
        Message::Sample(s) => s,
        _ => unreachable!(),
    };
    s.flags |= FLAG_ADC_ERROR;
    let mut stream = encode_msg(&Message::Sample(s), 0);
    // Dead-excitation sample: also invalid.
    let mut s2 = match sample(2) {
        Message::Sample(s) => s,
        _ => unreachable!(),
    };
    s2.excitation_raw = 0;
    stream.extend_from_slice(&encode_msg(&Message::Sample(s2), 1));
    p.ingest(&stream, now, &mut events);
    p.finalize(now);

    let snap = p.snapshot(now);
    assert_eq!(snap.samples_ok, 2, "invalid samples still decode");
    assert_eq!(snap.invalid_samples, 2);

    let dir = session_dir(&events);
    let samples = read(&dir, "samples.csv");
    let lines: Vec<&str> = samples.lines().collect();
    assert_eq!(
        lines.len(),
        1 + 2,
        "invalid samples are logged, not dropped"
    );
    for row in &lines[1..] {
        let cols: Vec<&str> = row.split(',').collect();
        let h: Vec<&str> = lines[0].split(',').collect();
        let idx = h.iter().position(|c| *c == "sample_valid").unwrap();
        assert_eq!(cols[idx], "0");
    }
}

// ---------------------------------------------------------------------
// Command windows and the transmit queue (half-duplex RS-485 rules).
// ---------------------------------------------------------------------

fn window_sample(seq_hint: u32) -> Message {
    match sample(seq_hint) {
        Message::Sample(mut s) => {
            s.flags |= FLAG_COMMAND_WINDOW;
            Message::Sample(s)
        }
        _ => unreachable!(),
    }
}

fn queued(desc: &str) -> QueuedCommand {
    QueuedCommand {
        message: Message::Query(Query { command_nonce: 1 }),
        desc: desc.into(),
    }
}

#[test]
fn queued_set_rtc_uses_physical_send_time() {
    let requested_ms = 1_772_000_000_000;
    let transmitted_ms = requested_ms + 987;
    let queued = Message::SetRtc(SetRtc {
        command_nonce: 77,
        unix_ms: requested_ms,
    });

    assert_eq!(
        prepare_message_for_send(queued, transmitted_ms),
        Message::SetRtc(SetRtc {
            command_nonce: 77,
            unix_ms: transmitted_ms,
        })
    );

    let query = Message::Query(Query { command_nonce: 78 });
    assert_eq!(prepare_message_for_send(query, transmitted_ms), query);
}

#[test]
fn ordinary_samples_never_open_a_command_window() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let now = Instant::now();

    let mut stream = encode_msg(&hello(), 0);
    for i in 1..=20u32 {
        stream.extend_from_slice(&encode_msg(&sample(i), i));
    }
    p.ingest(&stream, now, &mut events);
    assert!(
        !p.take_command_window(),
        "no window sample -> no transmission opportunity"
    );

    // And a queue holding commands must not release any of them.
    let mut q = CommandQueue::default();
    q.push(queued("SetRtc"));
    assert!(q.pop_for_window(false).is_none());
    assert_eq!(q.len(), 1, "command stays queued until a real window");
}

#[test]
fn command_window_flag_exposed_once_per_window_sample() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let now = Instant::now();

    let mut stream = encode_msg(&hello(), 0);
    stream.extend_from_slice(&encode_msg(&sample(1), 1));
    stream.extend_from_slice(&encode_msg(&window_sample(2), 2));
    p.ingest(&stream, now, &mut events);

    assert!(p.take_command_window(), "window sample opens the window");
    assert!(
        !p.take_command_window(),
        "reading the window consumes it — at most one transmission"
    );

    // A window sample *followed by more board traffic in the same batch*
    // means the window has already passed: do not transmit into it.
    let mut stream = encode_msg(&window_sample(3), 3);
    stream.extend_from_slice(&encode_msg(&sample(4), 4));
    p.ingest(&stream, now, &mut events);
    assert!(
        !p.take_command_window(),
        "board traffic after the window sample closes the window"
    );
}

#[test]
fn fifo_queue_pops_at_most_one_command_per_window() {
    let mut q = CommandQueue::default();
    assert_eq!(q.push(queued("first")), 1);
    assert_eq!(q.push(queued("second")), 2);
    assert_eq!(q.push(queued("third")), 3);

    // One window -> exactly the oldest command, FIFO order.
    let cmd = q.pop_for_window(true).expect("window releases one command");
    assert_eq!(cmd.desc, "first");
    assert_eq!(q.len(), 2, "only one command leaves per window");

    // Next windows drain in order.
    assert_eq!(q.pop_for_window(true).unwrap().desc, "second");
    assert_eq!(q.pop_for_window(true).unwrap().desc, "third");
    assert!(
        q.pop_for_window(true).is_none(),
        "empty queue, empty window"
    );
}

#[test]
fn sent_command_is_never_requeued_or_retried_without_ack() {
    let mut q = CommandQueue::default();
    q.push(queued("FirePulse run=9 nonce=46"));

    // The command is popped exactly once at its window...
    assert!(q.pop_for_window(true).is_some());
    // ...and no ack ever arrives. Nothing may bring it back: further
    // windows find the queue empty, forever.
    for _ in 0..10 {
        assert!(
            q.pop_for_window(true).is_none(),
            "a command is one intended transmission; no retry on missing ack"
        );
    }
    assert!(q.is_empty());
}

#[test]
fn disconnect_clears_unsent_queue() {
    let mut q = CommandQueue::default();
    q.push(queued("SetRtc"));
    q.push(queued("Query"));
    assert_eq!(q.clear(), 2, "disconnect drops every unsent command");
    assert!(q.is_empty());
    assert!(
        q.pop_for_window(true).is_none(),
        "nothing survives the disconnect"
    );
}

// ---------------------------------------------------------------------
// Simulator through the same pipeline, with synthetic time.
// ---------------------------------------------------------------------

/// Run the sim for `dur` of simulated time, feeding everything through the
/// pipeline in `step` increments.
fn run_sim(
    sim: &mut SimBoard,
    p: &mut Pipeline,
    events: &mut Vec<UiEvent>,
    t: &mut Instant,
    dur: Duration,
    step: Duration,
) {
    let end = *t + dur;
    let mut buf = Vec::new();
    while *t < end {
        *t += step;
        sim.poll(*t, &mut buf);
        if !buf.is_empty() {
            p.ingest(&buf, *t, events);
            buf.clear();
        }
    }
}

fn last_sample(events: &[UiEvent]) -> Option<&sap_flux_monitor::types::ProcessedSample> {
    events.iter().rev().find_map(|e| match e {
        UiEvent::SampleMsg(ps) => Some(ps.as_ref()),
        _ => None,
    })
}

fn last_status(events: &[UiEvent]) -> Option<&Status> {
    events.iter().rev().find_map(|e| match e {
        UiEvent::StatusMsg(st) => Some(st),
        _ => None,
    })
}

fn last_ack(events: &[UiEvent]) -> Option<&Ack> {
    events.iter().rev().find_map(|e| match e {
        UiEvent::AckMsg(a) => Some(a),
        _ => None,
    })
}

#[test]
fn simulator_advertises_full_capabilities_and_handles_commands_immediately() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let mut t = Instant::now();
    let mut sim = SimBoard::new(t, false);
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(2),
        Duration::from_millis(20),
    );

    // Full capability set, heater control included, so the entire safety
    // UX stays testable in simulator mode.
    let h = events
        .iter()
        .find_map(|e| match e {
            UiEvent::HelloMsg(h) => Some(*h),
            _ => None,
        })
        .expect("sim sends Hello");
    assert_eq!(
        h.capabilities,
        CAP_RTC | CAP_ADC | CAP_V24_MONITOR | CAP_HEATER_CONTROL
    );

    // The sim never marks command windows — it doesn't need them.
    for e in &events {
        if let UiEvent::SampleMsg(ps) = e {
            assert_eq!(
                ps.sample.flags & FLAG_COMMAND_WINDOW,
                0,
                "sim samples must not carry the command-window flag"
            );
        }
    }

    // Commands are consumed immediately: the ack appears in the very same
    // output buffer, with no window sample in between.
    let mut cmd_out = Vec::new();
    sim.handle_command(
        &Message::SetRtc(SetRtc {
            command_nonce: 77,
            unix_ms: 1_772_000_000_000,
        }),
        t,
        &mut cmd_out,
    );
    assert!(!cmd_out.is_empty(), "immediate response bytes");
    p.ingest(&cmd_out, t, &mut events);
    let ack = last_ack(&events).expect("immediate ack");
    assert_eq!(ack.command_nonce, 77);
    assert_eq!(ack.result, 0);
}

#[test]
fn simulator_full_pulse_cycle_through_pipeline() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let mut t = Instant::now();
    let mut sim = SimBoard::new(t, false);
    let step = Duration::from_millis(20);

    // Idle: hello + samples + status flow, RTC starts invalid.
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(2),
        step,
    );
    assert!(events.iter().any(|e| matches!(e, UiEvent::HelloMsg(_))));
    let dir = session_dir(&events);
    let ps = last_sample(&events).expect("samples flowing");
    assert_eq!(ps.sample.rtc_unix_ms, -1, "RTC invalid at boot");
    assert!(ps.sample_valid);
    assert!(ps.readings[0].valid(), "T1 valid");
    assert!(ps.readings[2].valid(), "all simulated channels are healthy");
    let temp = ps.readings[0].temp_c.unwrap();
    assert!(
        (20.0..30.0).contains(&temp),
        "plausible idle temp, got {temp}"
    );
    assert_eq!(last_status(&events).unwrap().state, BoardState::SafeIdle);

    // RTC transitions to valid on its own (~12 s).
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(11),
        step,
    );
    assert!(last_sample(&events).unwrap().sample.rtc_unix_ms > 0);

    // SetRtc is acked.
    let mut cmd_out = Vec::new();
    sim.handle_command(
        &Message::SetRtc(SetRtc {
            command_nonce: 42,
            unix_ms: 1_772_000_123_456,
        }),
        t,
        &mut cmd_out,
    );
    p.ingest(&cmd_out, t, &mut events);
    let ack = last_ack(&events).unwrap();
    assert_eq!(ack.command_nonce, 42);
    assert_eq!(ack.result, 0);

    // StartBaseline.
    let mut cmd_out = Vec::new();
    sim.handle_command(
        &Message::StartBaseline(StartBaseline {
            command_nonce: 43,
            run_id: 9,
            baseline_duration_ms: 60_000,
            recovery_duration_ms: 5_000,
            requested_sample_hz: 50,
        }),
        t,
        &mut cmd_out,
    );
    p.ingest(&cmd_out, t, &mut events);
    assert_eq!(last_ack(&events).unwrap().result, 0);
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(2),
        step,
    );
    let st = last_status(&events).unwrap();
    assert_eq!(st.state, BoardState::Baseline);
    assert_eq!(st.run_id, 9);

    // Arm with a wrong run id is rejected.
    let mut cmd_out = Vec::new();
    sim.handle_command(
        &Message::ArmPulse(ArmPulse {
            command_nonce: 44,
            run_id: 1,
            pulse_duration_ms: 500,
        }),
        t,
        &mut cmd_out,
    );
    p.ingest(&cmd_out, t, &mut events);
    assert_ne!(last_ack(&events).unwrap().result, 0);

    // Correct arm.
    let mut cmd_out = Vec::new();
    sim.handle_command(
        &Message::ArmPulse(ArmPulse {
            command_nonce: 45,
            run_id: 9,
            pulse_duration_ms: 500,
        }),
        t,
        &mut cmd_out,
    );
    p.ingest(&cmd_out, t, &mut events);
    assert_eq!(last_ack(&events).unwrap().result, 0);
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(1),
        step,
    );
    assert_eq!(last_status(&events).unwrap().state, BoardState::Armed);

    // Fire: 500 ms pulse with heater flag, then recovery, then cooldown.
    let mut cmd_out = Vec::new();
    sim.handle_command(
        &Message::FirePulse(FirePulse {
            command_nonce: 46,
            run_id: 9,
        }),
        t,
        &mut cmd_out,
    );
    p.ingest(&cmd_out, t, &mut events);
    assert_eq!(last_ack(&events).unwrap().result, 0);

    events.clear();
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_millis(400),
        step,
    );
    let ps = last_sample(&events).unwrap();
    assert_ne!(
        ps.sample.flags & FLAG_HEATER_ACTIVE,
        0,
        "heater on during pulse"
    );
    assert_eq!(ps.sample.state, BoardState::Pulsing);

    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(2),
        step,
    );
    let ps = last_sample(&events).unwrap();
    assert_eq!(
        ps.sample.flags & FLAG_HEATER_ACTIVE,
        0,
        "heater off after pulse"
    );
    assert_eq!(last_status(&events).unwrap().state, BoardState::Recovery);

    // Heat trace: downstream middle (T2) should have risen noticeably.
    let ps = last_sample(&events).unwrap();
    let t2 = ps.readings[1].temp_c.unwrap();
    assert!(t2 > 24.8, "pulse heat visible on T2, got {t2}");

    // After recovery ends -> Cooldown with a board-reported countdown.
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(5),
        step,
    );
    let st = last_status(&events).unwrap();
    assert_eq!(st.state, BoardState::Cooldown);
    assert!(
        st.cooldown_remaining_ms > 500_000,
        "10-minute cooldown reported"
    );

    p.finalize(t);
    let samples = read(&dir, "samples.csv");
    assert!(samples.lines().count() > 100);
}

#[test]
fn simulator_error_injection_exercises_counters() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let mut t = Instant::now();
    let mut sim = SimBoard::new(t, true);
    // ~40 s of simulated time at 10 Hz -> ~400 samples -> several injected
    // CRC errors and at least one sequence gap.
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(40),
        Duration::from_millis(20),
    );
    let snap = p.snapshot(t);
    assert!(
        snap.crc_errors + snap.framing_errors >= 2,
        "injected corruption detected: {snap:?}"
    );
    assert!(snap.seq_gaps >= 1, "injected gaps detected: {snap:?}");
    assert!(
        snap.samples_ok > 300,
        "stream keeps flowing between injected errors"
    );
}

#[test]
fn abort_during_baseline_returns_to_safe_idle() {
    let root = Scratch::new();
    let mut p = test_pipeline(&root);
    let mut events = Vec::new();
    let mut t = Instant::now();
    let mut sim = SimBoard::new(t, false);
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(1),
        Duration::from_millis(20),
    );

    let mut cmd_out = Vec::new();
    sim.handle_command(
        &Message::StartBaseline(StartBaseline {
            command_nonce: 1,
            run_id: 5,
            baseline_duration_ms: 60_000,
            recovery_duration_ms: 60_000,
            requested_sample_hz: 20,
        }),
        t,
        &mut cmd_out,
    );
    sim.handle_command(
        &Message::Abort(Abort {
            command_nonce: 2,
            run_id: 5,
        }),
        t,
        &mut cmd_out,
    );
    p.ingest(&cmd_out, t, &mut events);
    assert_eq!(last_ack(&events).unwrap().result, 0);
    run_sim(
        &mut sim,
        &mut p,
        &mut events,
        &mut t,
        Duration::from_secs(2),
        Duration::from_millis(20),
    );
    assert_eq!(last_status(&events).unwrap().state, BoardState::SafeIdle);
}
