//! End to end: real server (UDP + API) on localhost, driven by the gateway
//! simulator.

use http_body_util::BodyExt;
use sapflux_server::config::Config;
use sapflux_server::sim::{capture_window, synthetic_window, Gateway, SimBoard};
use sapflux_server::{api, start, window, Running};
use sapflux_uplink::{
    self as up, Body, Header, Schedule, WindowPart, MAX_DATAGRAM_LEN, REQUEST_REBOOT, REQUEST_SEND_WINDOWS,
};
use std::time::Duration;
use tower::ServiceExt;

const DEVICE: u64 = 352_656_100_000_001;
const BOARD: u64 = 0xD15C_55B4_116E_105E;
const KEY_HEX: &str = "4f1c2b3a5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f701";

fn key() -> Vec<u8> {
    sapflux_server::board::unhex(KEY_HEX).unwrap()
}

async fn server(dir: &std::path::Path) -> (Running, Config) {
    let cfg = Config::parse(&format!(
        r#"
[server]
udp_bind = "127.0.0.1:0"
api_bind = "127.0.0.1:0"
data_dir = "{}"
[defaults]
enabled = true
[[device]]
name = "box1"
device_id = {DEVICE}
key = "{KEY_HEX}"
"#,
        dir.display()
    ))
    .unwrap();
    (start(&cfg).await.unwrap(), cfg)
}

async fn gateway(r: &Running, boot_id: u32) -> Gateway {
    let mut g = Gateway::new(r.udp_addr, DEVICE, key(), boot_id).await.unwrap();
    g.timeout = Duration::from_millis(800);
    g
}

#[tokio::test(flavor = "multi_thread")]
async fn full_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _cfg) = server(dir.path()).await;
    let mut gw = gateway(&r, 0x1111_0001).await;
    gw.clock_skew_ms = 5_000;
    let mut brd = SimBoard::new(BOARD);

    // Boot event.
    gw.event(up::EVENT_BOOT, "boot").await.unwrap();

    // First check-in: time sync, defaults pushed to a factory-default board.
    let c = gw.check_in(&mut brd).await.unwrap();
    assert!((c.offset_ms + 5_000).abs() < 100, "offset {}", c.offset_ms);
    assert!(c.rtt_ms >= 0 && c.rtt_ms < 1000);
    assert!(c.reply.t2_unix_ms <= c.reply.t3_unix_ms);
    let want = Schedule {
        enabled: 1,
        ..Schedule::FACTORY_DEFAULT
    };
    assert_eq!(c.reply.schedule, want);
    assert_eq!(c.reply.config_version, 1);
    assert_eq!(c.reply.wake_interval_s, 3600);
    assert_eq!(c.reply.lease_s, 10_800);
    assert_eq!(c.reply.request_flags, REQUEST_SEND_WINDOWS);
    assert!(c.pushed_schedule);
    // Second check-in: board now reports the server's schedule_crc.
    let c = gw.check_in(&mut brd).await.unwrap();
    assert!(!c.pushed_schedule);
    {
        let s = r.store.lock().unwrap();
        let d = s.device(DEVICE).unwrap();
        assert_eq!(d.schedule_in_sync, Some(true));
        assert_eq!(d.board_id.as_deref(), Some("D15C55B4116E105E"));
        assert!(d.field_status.is_some());
        assert_eq!(d.batt_mv, Some(3310));
    }

    // Window with parts 1 and 7 lost on the first pass.
    let run = 1_792_000_000u32;
    let (bytes, _) = synthetic_window(run, 1_792_000_000_000, &want);
    assert_eq!(bytes.len(), 12_634);
    let (ack, sent) = gw.send_window(BOARD, run, &bytes, &[1, 7], 3).await.unwrap();
    assert!(ack.complete);
    assert_eq!(ack.part_count, 12);
    assert_eq!(sent, 12, "10 parts + 2 resends");
    let row = r.store.lock().unwrap().window("D15C55B4116E105E", run, None).unwrap().unwrap();
    assert_eq!(row.status, "ok");
    assert_eq!(row.n_bins, Some(482));
    assert_eq!(std::fs::read(format!("{}.bin", row.stem)).unwrap(), bytes);
    let csv = std::fs::read_to_string(format!("{}.csv", row.stem)).unwrap();
    assert_eq!(csv.lines().count(), 483);
    assert!(std::path::Path::new(&format!("{}.json", row.stem)).exists());
    // Decoded CSV values equal the binned values.
    let (info, data) = window::verify(&bytes, Some(run)).unwrap();
    let d = window::decode(&info, data);
    let line200: Vec<&str> = csv.lines().nth(201).unwrap().split(',').collect();
    assert_eq!(line200[0], "200");
    let p1t1: f64 = line200[5].parse().unwrap();
    assert!((p1t1 - d.bins[200].raw[0].unwrap()).abs() < 1e-3);

    // Resending a part of a completed window is acknowledged as complete
    // and stores nothing new.
    let (ack, _) = gw.send_window(BOARD, run, &bytes, &(0..12).filter(|&i| i != 3).collect::<Vec<_>>(), 1)
        .await
        .unwrap();
    assert!(ack.complete);
    assert_eq!(r.store.lock().unwrap().windows(None, 10).unwrap().len(), 1);

    // Replay of an old datagram: dropped, no reply, no new event.
    let events_before = r.store.lock().unwrap().events(Some(DEVICE), 100).unwrap().len();
    let old = gw.sent[0].clone();
    assert!(gw.send_raw(&old, Duration::from_millis(300)).await.unwrap().is_none());
    assert_eq!(r.store.lock().unwrap().events(Some(DEVICE), 100).unwrap().len(), events_before);

    // Bad MAC and unknown device: dropped.
    let mut bad = gw.sent.last().unwrap().clone();
    *bad.last_mut().unwrap() ^= 0x55;
    assert!(gw.send_raw(&bad, Duration::from_millis(300)).await.unwrap().is_none());
    let mut stranger = Gateway::new(r.udp_addr, DEVICE + 1, key(), 9).await.unwrap();
    stranger.timeout = Duration::from_millis(200);
    stranger.retries = 0;
    assert!(stranger.event(1, "hi").await.is_err());

    // Remote off switch.
    r.store.lock().unwrap().set_enabled(DEVICE, false, "test").unwrap();
    let c = gw.check_in(&mut brd).await.unwrap();
    assert_eq!(c.reply.schedule.enabled, 0);
    assert_eq!(c.reply.config_version, 2);
    assert!(c.pushed_schedule);
    assert_eq!(brd.schedule.enabled, 0);

    // Reboot request: flagged until the gateway comes back with a new boot_id.
    r.store.lock().unwrap().request_reboot(DEVICE, "test").unwrap();
    let c = gw.check_in(&mut brd).await.unwrap();
    assert_ne!(c.reply.request_flags & REQUEST_REBOOT, 0);
    let c = gw.check_in(&mut brd).await.unwrap();
    assert_ne!(c.reply.request_flags & REQUEST_REBOOT, 0);
    let mut gw2 = gateway(&r, 0x1111_0002).await;
    let c = gw2.check_in(&mut brd).await.unwrap();
    assert_eq!(c.reply.request_flags & REQUEST_REBOOT, 0);
    assert!(!r.store.lock().unwrap().settings(DEVICE).unwrap().reboot_pending);
    // A fresh boot starts at seq 1 again and is accepted.
    assert_eq!(gw2.seq, 1);

    // Same run_id, different content: stored as a new generation.
    let (bytes2, _) = synthetic_window(run, 1_792_000_100_000, &Schedule { recovery_s: 120, ..want });
    let (ack, _) = gw2.send_window(BOARD, run, &bytes2, &[], 2).await.unwrap();
    assert!(ack.complete);
    let row = r.store.lock().unwrap().window("D15C55B4116E105E", run, None).unwrap().unwrap();
    assert_eq!(row.generation, 1);
    assert!(row.stem.ends_with("_g1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_window_retried_then_stored_bad() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _cfg) = server(dir.path()).await;
    let gw = gateway(&r, 7).await;
    let s = Schedule {
        enabled: 1,
        ..Schedule::FACTORY_DEFAULT
    };
    let (mut bytes, _) = synthetic_window(42, 1_792_000_000_000, &s);
    let n = bytes.len();
    bytes[n - 10] ^= 0xFF; // data no longer matches data_crc32
    let part_count = n.div_ceil(1100) as u16;

    // Send all parts directly: each full pass fails verification and the
    // server clears the bitmap, until the third failure.
    let mut seq = 0;
    let mut last = None;
    for pass in 0..3 {
        for (i, chunk) in bytes.chunks(1100).enumerate() {
            seq += 1;
            let body = Body::WindowPart(WindowPart {
                board_id: BOARD,
                run_id: 42,
                part: i as u16,
                part_count,
                total_len: n as u32,
                data: chunk,
            });
            let mut out = [0u8; MAX_DATAGRAM_LEN];
            let len = up::seal(
                &Header {
                    msg_type: 0,
                    device_id: DEVICE,
                    boot_id: 7,
                    seq,
                },
                &body,
                &key(),
                &mut out,
            )
            .unwrap();
            let reply = gw.send_raw(&out[..len], Duration::from_secs(2)).await.unwrap().unwrap();
            let Body::WindowAck(a) = up::open(&reply, &key()).unwrap().1 else { panic!() };
            last = Some(a);
        }
        let a = last.unwrap();
        if pass < 2 {
            assert!(!a.complete, "pass {pass}");
            assert_eq!(a.received_bitmap, [0; 32], "bitmap cleared to force a full resend");
        }
    }
    assert!(last.unwrap().complete);
    let row = r.store.lock().unwrap().window("D15C55B4116E105E", 42, None).unwrap().unwrap();
    assert_eq!(row.status, "bad");
    assert!(row.error.unwrap().contains("data_crc32"));
    assert!(std::path::Path::new(&format!("{}.bin", row.stem)).exists());
    let ev = r.store.lock().unwrap().events(Some(DEVICE), 10).unwrap();
    assert_eq!(ev.iter().filter(|e| e.code == 1002).count(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn real_capture_window() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../software/sap-flux-monitor/captures/session_20260824_203902/samples.csv"
    ));
    if !path.exists() {
        eprintln!("capture missing; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (r, _cfg) = server(dir.path()).await;
    let mut gw = gateway(&r, 3).await;
    let (run, bytes) = capture_window(path).unwrap();
    let (ack, _) = gw.send_window(BOARD, run, &bytes, &[4], 3).await.unwrap();
    assert!(ack.complete);
    let row = r.store.lock().unwrap().window("D15C55B4116E105E", run, None).unwrap().unwrap();
    assert_eq!(row.status, "ok");
    let (info, data) = window::verify(&bytes, Some(run)).unwrap();
    let d = window::decode(&info, data);
    // The capture's TIM2/HSI timebase ran ~0.03-0.04 % slow against the RTC
    // (docs/field-gateway-design.md); the window's clock ratio shows it.
    assert!(d.clock.rtc_valid);
    assert!((d.clock.ratio - 1.0).abs() < 1e-3, "ratio {}", d.clock.ratio);
    // Probe 2 is absent from the capture: all sentinel.
    assert!(d.bins.iter().all(|b| b.raw[6..12].iter().all(Option::is_none)));
    // Downstream middle thermistor (T2) warms after the pulse.
    let t = |k: usize| {
        let b = &d.bins[k];
        window::temp_c(b.raw[12].unwrap(), b.raw[1].unwrap()).unwrap()
    };
    let before = t(100);
    let peak = (120..300).map(t).fold(f64::MIN, f64::max);
    assert!(peak - before > 0.5, "rise {}", peak - before);
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_api() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _cfg) = server(dir.path()).await;
    let mut gw = gateway(&r, 5).await;
    let mut brd = SimBoard::new(BOARD);
    gw.check_in(&mut brd).await.unwrap(); // pushes the schedule
    gw.check_in(&mut brd).await.unwrap(); // board reports it back
    let s = Schedule {
        enabled: 1,
        ..Schedule::FACTORY_DEFAULT
    };
    let (bytes, _) = synthetic_window(77, 1_792_000_000_000, &s);
    gw.send_window(BOARD, 77, &bytes, &[], 2).await.unwrap();

    let app = api::router(r.store.clone());
    let call = |method: &str, uri: &str, body: Option<&str>| {
        let mut req = axum::http::Request::builder().method(method).uri(uri);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        let req = req
            .body(axum::body::Body::from(body.unwrap_or("").to_string()))
            .unwrap();
        let app = app.clone();
        async move {
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status().as_u16();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).to_string())
        }
    };

    let (st, body) = call("GET", "/api/devices", None).await;
    assert_eq!(st, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["name"], "box1");
    assert_eq!(v[0]["schedule_in_sync"], true);
    assert_eq!(v[0]["field_status"]["schedule_crc"], v[0]["settings"]["schedule_crc"]);

    let (st, body) = call("POST", "/api/devices/box1/disable", None).await;
    assert_eq!(st, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["enabled"], false);
    assert_eq!(v["config_version"], 2);
    let (st, _) = call("POST", &format!("/api/devices/{DEVICE}/enable"), None).await;
    assert_eq!(st, 200);

    let (st, body) = call("PUT", "/api/devices/box1/schedule", Some(r#"{"interval_s": 1800, "phase_s": 900}"#)).await;
    assert_eq!(st, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["interval_s"], 1800);
    assert_eq!(v["config_version"], 4);
    // Board limits enforced: interval < baseline + recovery + 600 s cooldown.
    let (st, body) = call("PUT", "/api/devices/box1/schedule", Some(r#"{"interval_s": 800}"#)).await;
    assert_eq!(st, 422, "{body}");
    let (st, _) = call("PUT", "/api/devices/box1/schedule", Some(r#"{"pulse_duration_ms": 2000}"#)).await;
    assert_eq!(st, 422);
    let (st, _) = call("GET", "/api/devices/nope", None).await;
    assert_eq!(st, 404);

    // SFK1 needs a probe kind; the kinds and SFK1 travel in schedule flags,
    // the orientation only in server settings.
    let (st, _) = call("PUT", "/api/devices/box1/schedule", Some(r#"{"sfk1": true}"#)).await;
    assert_eq!(st, 422);
    let (st, body) = call(
        "PUT",
        "/api/devices/box1/schedule",
        Some(r#"{"sfk1": true, "probe1": "east30", "probe2": "east30_offset", "probe2_flipped": true}"#),
    )
    .await;
    assert_eq!(st, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!((v["probe2"].as_str(), v["probe2_flipped"].as_bool()), (Some("east30_offset"), Some(true)));
    let s = r.store.lock().unwrap().settings(DEVICE).unwrap();
    let wire = s.schedule();
    assert!(wire.sfk1());
    assert_eq!(wire.probe(0), sap_flux_protocol::ProbeKind::East30);
    assert_eq!(wire.probe(1), sap_flux_protocol::ProbeKind::East30Offset);
    let (st, _) = call("PUT", "/api/devices/box1/schedule", Some(r#"{"probe1": "rubber"}"#)).await;
    assert_eq!(st, 422);
    let (st, _) = call(
        "PUT",
        "/api/devices/box1/schedule",
        Some(r#"{"sfk1": false, "probe1": "none", "probe2": "none", "probe2_flipped": false}"#),
    )
    .await;
    assert_eq!(st, 200);

    let (st, body) = call("GET", "/api/windows", None).await;
    assert_eq!(st, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["run_id"], 77);
    let (st, csv) = call("GET", "/api/windows/D15C55B4116E105E/77/csv", None).await;
    assert_eq!(st, 200);
    assert_eq!(csv.lines().count(), 483);
    let (st, _) = call("GET", "/api/windows/D15C55B4116E105E/78/csv", None).await;
    assert_eq!(st, 404);

    // The next check-in carries the new config.
    let c = gw.check_in(&mut brd).await.unwrap();
    assert_eq!(c.reply.schedule.interval_s, 1800);
    assert_eq!(c.reply.schedule.phase_s, 900);
    assert_eq!(c.reply.config_version, 6); // two more for the SFK1 round trip
    assert_eq!(c.reply.schedule.flags, 0);
}
