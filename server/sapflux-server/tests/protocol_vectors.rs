//! Cross-check the server's board-frame and window decoding against the
//! golden vectors committed by the protocol crate
//! (`software/sap-flux-protocol/tests/vectors/field_v1.txt`).

use sapflux_server::board::{self, unhex, BoardMsg};
use sapflux_server::window;
use sapflux_uplink::{Schedule, SCHEDULE_LEN};
use std::collections::HashMap;

fn vectors() -> Option<String> {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../software/sap-flux-protocol/tests/vectors/field_v1.txt"
    );
    std::fs::read_to_string(p).ok()
}

#[test]
fn field_vectors_decode() {
    let Some(text) = vectors() else {
        eprintln!("vectors file missing; skipping");
        return;
    };
    let mut frames: HashMap<String, Vec<u8>> = HashMap::new();
    let mut data: HashMap<String, (Vec<u8>, u32)> = HashMap::new();
    let mut schedules = 0;
    for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "frame" => {
                let bytes = unhex(f[3]).unwrap();
                // Every frame must decode; board->host ones must be typed.
                let fr = board::parse_frame(&bytes).unwrap_or_else(|e| panic!("{}: {e}", f[1]));
                assert_eq!(fr.sequence, f[2].parse::<u32>().unwrap());
                frames.insert(f[1].to_string(), bytes);
            }
            "schedule" => {
                let b: [u8; SCHEDULE_LEN] = unhex(f[2]).unwrap().try_into().unwrap();
                let s = Schedule::from_bytes(&b);
                assert_eq!(format!("{:04x}", s.crc16()), f[3], "{}", f[1]);
                assert_eq!(s.to_bytes(), b);
                schedules += 1;
            }
            "window_data" => {
                data.insert(
                    f[1].to_string(),
                    (unhex(f[2]).unwrap(), u32::from_str_radix(f[3], 16).unwrap()),
                );
            }
            _ => {}
        }
    }
    assert_eq!(schedules, 2);

    match board::parse_frame(&frames["FieldStatus"]).unwrap().msg {
        BoardMsg::FieldStatus(fs) => assert_eq!(fs.flag_names().len(), fs.schedule_flags.count_ones() as usize),
        m => panic!("{m:?}"),
    }

    // WindowInfo frame + its data form a valid reassembled window.
    let wi_frame = &frames["WindowInfo"];
    let info = match board::parse_frame(wi_frame).unwrap().msg {
        BoardMsg::WindowInfo(w) => w,
        m => panic!("{m:?}"),
    };
    let (d, crc) = &data["WindowInfo"];
    assert_eq!(info.data_crc32, *crc);
    let mut bytes = wi_frame.clone();
    bytes.extend_from_slice(d);
    let (vi, vdata) = window::verify(&bytes, Some(info.run_id)).unwrap();
    let dec = window::decode(&vi, vdata);
    assert_eq!(dec.bins.len(), info.n_bins as usize);
    // Independent decode straight from the bytes: raw = base + v/16.
    for (k, b) in dec.bins.iter().enumerate() {
        for c in 0..info.channels as usize {
            let off = (k * info.channels as usize + c) * 2;
            let v = i16::from_le_bytes([d[off], d[off + 1]]);
            let expect = (v != i16::MIN).then(|| info.base_raw[c] as f64 + v as f64 / 16.0);
            assert_eq!(b.raw[c], expect, "bin {k} ch {c}");
        }
    }
    // The chunk vectors reassemble to the same data.
    let mut from_chunks = Vec::new();
    for name in ["WindowChunk_0", "WindowChunk_last"] {
        let f = &frames[name];
        let len = u16::from_le_bytes([f[8 + 6], f[8 + 7]]) as usize;
        from_chunks.extend_from_slice(&f[8 + 8..8 + 8 + len]);
    }
    assert_eq!(&from_chunks, d);
}
