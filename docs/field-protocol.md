# Field protocol: board ↔ Feather ↔ server

Contract for the first field deployment (one board, two probes, one Feather).
Board firmware, Feather firmware, the server and the host monitor all build
against this file. Change it here first.

Decisions this spec encodes (2026-10-24):

- **The board runs the schedule by itself.** It keeps pulsing on its last
  stored schedule whether or not the gateway or server is reachable. The
  server's `enabled = 0` turns it off at the next contact. A strict lease
  (stop when not renewed) exists as an opt-in flag, off by default.
- **The board keeps its own best time:** RTC on the LSE crystal, CR2032
  backup, RTC_CALR trim learned from `SetRtc` corrections and stored in
  EEPROM, and an LSE-clocked LPTIM1 timebase for sampling.
- **Uplink tier for day one:** per pulse, a 2 Hz binned window of all 12
  thermistors plus excitation. Fitting happens on the server. Raw 25 Hz frames
  go to SPI flash when that lands.

## 1. Board ↔ Feather (J6, LPUART)

Same framing as protocol v1 (`software/sap-flux-protocol`): COBS, `0x00`
delimiter, 8-byte header, CRC-32, max decoded frame 128 bytes. Protocol
version stays 1; the messages below are new type codes. All integers
little-endian.

Physical: board J6 p2 (board RX, PA13) ← 1 kΩ ← Feather TX (P0.24); J6 p3
(board TX, PA14) → 1 kΩ → Feather RX (P0.23); J6 p4 GND. 115200 8N1. No
pull-up on Feather RX (PA14 has the 10 kΩ BOOT0 pull-down).

### Link behavior in field mode (PA6 strapped low)

- The board sends nothing unsolicited on the LPUART. No Sample streaming.
- The board leaves PA14 as input/analog when not transmitting (the BOOT0
  pull-down otherwise costs 330 µA with TX idle high). Re-enable TX just for
  a reply.
- The Feather wakes the board with a single `0x00` byte, waits ≥ 10 ms, then
  sends commands. Each command gets a reply within 200 ms (one 2 Hz window
  chunk at a time). The Feather retries a command up to 3 times.
- The board accepts LPUART commands at any time except while the heater is
  on; commands that arrive during the pulse are answered after it (≤ 1.5 s).
- RS-485 (J5) keeps its v1 bench behavior. In field mode the SP3485 is held
  in shutdown (~RE high, DE low) unless the RS-485 transport is selected.

### New host → board commands

| type | name | payload |
|---|---|---|
| `0x07` | `SetSchedule` | 32 B |
| `0x08` | `GetWindow` | 14 B |
| `0x09` | `AckWindow` | 12 B |
| `0x0A` | `GetFieldStatus` | 8 B |
| `0x0B` | `GetRaw` | 16 B |

`SetSchedule` (32 B): `command_nonce: u64`, `enabled: u8`, `flags: u8`,
`pulse_duration_ms: u16`, `interval_s: u32`, `phase_s: u32`,
`baseline_s: u16`, `recovery_s: u16`, `sample_hz: u16`, `bin_ms: u16`,
`lease_until_unix_s: u32`.

- `flags` bit 0 `REQUIRE_LEASE`: only pulse while `now < lease_until`.
  Default 0.
- `flags` bit 1 `SFK1`: windows carry SFK1 records instead of bins (see
  SFK1 window data below). Needs `sample_hz` 25 and at least one probe
  kind.
- `flags` bits 2–3 / 4–5: probe 1 / probe 2 kind. 0 none, 1 East30 (both
  needles 6 mm from the heater), 2 East30 offset (10 mm and 5 mm, either
  way up); 3 is rejected. The kind picks the SFK1 profile; binned windows
  ignore it. Bits 6–7 zero.
- Pulses start at RTC times `t` with `(t - phase_s) mod interval_s == 0`
  (Unix seconds); `t` is the heater-on time, baseline starts `baseline_s`
  earlier. If the RTC is invalid the board pulses every `interval_s` on its
  monotonic clock and marks windows `TIME_INVALID`.
- Limits (reject with `INVALID_PARAMETER` 0x02): `pulse_duration_ms` ∈ {500,
  1000, 1500}; `interval_s` ≥ 600; `baseline_s` 10–300; `recovery_s`
  60–600; `baseline_s + recovery_s + cooldown` < `interval_s`; `sample_hz` ∈
  {10, 20, 25}; `bin_ms` ∈ {250, 500, 1000}.
- Persisted in EEPROM (write only when a non-lease field changes; lease is
  kept in RAM and a TAMP backup register).
- Factory default (blank EEPROM): disabled, 1000 ms, 3600 s, phase 0, 60 s,
  180 s, 25 Hz, 500 ms.
- Reply: `Ack` (result 0 or code) then `FieldStatus`.

`GetWindow` (14 B): `command_nonce: u64`, `run_id: u32` (0 = oldest
undelivered), `chunk: u16` (`0xFFFF` = header). Reply: `WindowInfo` for
`0xFFFF`, else `WindowChunk`. Unknown run or chunk: `Ack` with result `0x04`
(`NOT_FOUND`).

`AckWindow` (12 B): `command_nonce: u64`, `run_id: u32`. Marks the window
delivered and frees it. Reply `Ack`.

`GetFieldStatus` (8 B): `command_nonce: u64`. Reply `FieldStatus`.
`Query` keeps replying `Hello` + `Status` (+ `FieldStatus` in field mode).

`GetRaw` (16 B): `command_nonce: u64`, `run_id: u32`, `chunk: u32`. Reads
the raw 25 Hz frame log in flash (bench retrieval; the gateway does not use
it). Reply `RawChunk` with bytes `[100·chunk, 100·chunk + len)` of the run's
block stream; an unknown or overwritten run, a chunk past the end, or a
flash fault gets `Ack` `NOT_FOUND`; flash too busy to read in time (an
erase in flight) gets `Ack` `BUSY` (0x05), and the host retries that chunk.
The host reads chunks 0, 1, … until `len < 100` or `NOT_FOUND`. On RS-485 a `GetRaw` while the heater is on
gets `Ack` `WRONG_STATE` (0x10).

### New board → host messages

| type | name | payload |
|---|---|---|
| `0x15` | `Sample12` | 52 B |
| `0x16` | `WindowInfo` | 90 B |
| `0x17` | `WindowChunk` | 108 B |
| `0x18` | `FieldStatus` | 52 B |
| `0x19` | `RawChunk` | 112 B |

`Sample12` (52 B), for bench streaming and the monitor:
`monotonic_us: u64`, `rtc_unix_ms: i64`, `run_id: u32`, `flags: u16`,
`state: u8`, `reserved: u8`, `excitation_raw: i16`, `v24_raw: i16`,
`thermistor_raw: [i16; 12]` (probe 1 T1..T6, probe 2 T1..T6).

`WindowInfo` (90 B): `run_id: u32`, `window_flags: u16`, `channels: u8`
(13), `bin_ms_div10: u8` (50 = 500 ms), `n_bins: u16`, `chunk_count: u16`,
`start_monotonic_us: u64`, `start_rtc_unix_ms: i64`,
`end_monotonic_us: u64`, `end_rtc_unix_ms: i64`, `heater_on_offset_ms:
u32` (from window start), `pulse_duration_ms: u16`, `sample_hz: u16`,
`v24_mean_raw: i16`, `frames_total: u16`, `frames_missing: u16`,
`data_crc32: u32`, `base_raw: [i16; 13]`, `reserved: u16` (zero).

- `window_flags`: bit 0 `TIME_INVALID`, bit 1 `ADC_ERRORS`, bit 2
  `ABORTED`, bit 3 `HEATER_FAULT`, bit 4 `TRUNCATED`, bit 5 `SFK1`.
- Monotonic and RTC stamps at both ends let the server rescale time.

Window data: `n_bins` bins, each bin `channels` × `i16` in order
probe-1 T1..T6, probe-2 T1..T6, excitation. Value =
`clamp(round(16 × (bin_mean_raw − base_raw[c])), −32767, 32767)` (round
half away from zero; −32768 is reserved for the missing sentinel), where
`base_raw[c]` is the rounded mean raw count of channel `c` in the first bin,
or 0 if that bin has no valid frames for the channel.
A bin with no valid frames for a channel stores `−32768`. `data_crc32` is
CRC-32 (protocol CRC) over the full data byte string. Bin `k` covers
`[start + k·bin_ms, start + (k+1)·bin_ms)` on the monotonic clock.

SFK1 window data (`window_flags` bit 5): one entry per probe whose kind is
set, in probe order: `probe: u8` (1 or 2), `len: u16`, then `len` bytes
of SFK1 record (`software/sfk1`) of that probe's T1..T6, ratiometric
against the excitation channel. Classic probes use profile `East30K12`,
offset probes `East30Offset`; each record has the extended header (6
channels), which names its profile. `len = 0`: the probe produced no record
(no valid baseline frame). In an SFK1 window `n_bins` is the data length in
bytes and `bin_ms_div10` and `base_raw` are zero. Frames are on the 40 ms
grid: frame `i` of each record is at `start_monotonic_us + i × 40 ms`
(start is the record's first frame, not the baseline start), record time 0
is `n_baseline_bins × 5 s` after start (`min(12, baseline_s / 5)` bins), and
the heater actually came on `heater_on_offset_ms` after start, so a decoder
shifts the record by the difference. A frame with a failed read of the
excitation or any of a probe's thermistors is a missing frame of that
probe's record; frames past the record's 181 s after onset are not encoded.
About 160 B per probe, 2 chunks per window for two probes.

`WindowChunk` (108 B): `run_id: u32`, `chunk: u16`, `len: u16`
(≤ 100), `data: [u8; 100]` (zero-padded). Chunk `i` holds data bytes
`[100·i, 100·i + len)`. `chunk_count = ceil(total_bytes / 100)`.

Default size: (60 + 1 + 180) s × 2 bins/s ≈ 482 bins × 13 × 2 B ≈ 12.5 kB,
126 chunks. A finished window is written to one of 256 SPI-flash slots
(read back and verified, then freed from RAM) and served from flash, so a
board reset loses no undelivered window. `AckWindow` marks the slot
delivered. When all 256 slots hold undelivered windows, the next window
overwrites the oldest (`windows_dropped`). Without a working flash the board
keeps the newest 4 undelivered windows in RAM, as before.

`FieldStatus` (52 B): `monotonic_us: u64`, `rtc_unix_ms: i64`,
`next_pulse_unix_s: u32` (0 if disabled), `schedule_flags: u8` (bit 0
enabled, bit 1 require-lease, bit 2 lease-valid, bit 3 RTC-valid, bit 4
heater-lockout, bit 5 flash-ok, bit 6 EEPROM fault: schedule held),
`windows_pending: u8`, `windows_dropped: u16`, `oldest_pending_run_id:
u32`, `last_run_id: u32`, `calr_ppb: i32` (current RTC trim, parts per billion, positive = slowed), `last_rtc_step_ms:
i32` (last `SetRtc` correction, new − old), `boot_count: u16`,
`v24_raw: i16`, `flash_used_kib: u32`, `schedule_crc: u16`, `reserved: u16`.

`schedule_crc` = low 16 bits of CRC-32 over the 20 schedule bytes of the
last accepted `SetSchedule` (`enabled` through `bin_ms`), so the gateway can
tell whether the board already has the server's config.

`RawChunk` (112 B): `run_id: u32`, `chunk: u32`, `len: u16` (≤ 100),
`reserved: u16` (zero), `data: [u8; 100]` (zero-padded).

Raw block stream: the run's 256-byte flash blocks in order, each holding up
to 12 frames of every value a `Sample12` carries (12 thermistors,
excitation, v24, low byte of `flags`, `state`), from Baseline through the
end of Recovery. Header: `magic: u16` (`0x4652`), `version: u8` (1),
`n_frames: u8`, `run_id: u32`, `seq: u32`, `block_in_run: u32`,
`first_monotonic_us: u64`, `first_rtc_unix_ms: i64` (−1 = invalid); then
the frames (33 B absolute, 17 B delta: i8 differences from the previous
frame), `0xFF` padding, and a CRC-32 over bytes 0..252 in the last 4 bytes.
The exact frame encoding is `software/sap-flux-protocol/src/raw.rs`.
A frame the board could not queue for flash in time is dropped, so
`monotonic_us` gaps are possible.

### Time on the board

- `SetRtc` keeps its v1 semantics (SafeIdle only; the board steps on the
  next second). It is now also accepted in Cooldown and between windows; it
  is never applied during a window.
- The board stores `(rtc_ms, monotonic_us)` at every accepted `SetRtc`. When
  two corrections are ≥ 6 h apart, `drift = step / elapsed`; update RTC_CALR
  by a smoothed (α = 0.5) estimate, clamp to ±200 ppm, persist the trim in
  EEPROM, and restore it at boot.
- The Feather sends `SetRtc` only when |board − server time| > 250 ms.

## 2. Feather ↔ server (UDP)

One request/response exchange per datagram. Datagrams ≤ 1200 bytes. The
server address, port, device id and key are compile-time config read from a
gitignored file (never committed).

### Datagram layout

| offset | size | field |
|---|---|---|
| 0 | 2 | magic `b"SF"` |
| 2 | 1 | version = 1 |
| 3 | 1 | type |
| 4 | 8 | `device_id: u64` (gateway; from the IMEI) |
| 12 | 4 | `boot_id: u32` (random per gateway boot) |
| 16 | 4 | `seq: u32` (per boot, starts at 1) |
| 20 | n | body |
| 20+n | 8 | HMAC-SHA256(key, bytes `0..20+n`) truncated to 8 bytes |

The server drops datagrams with a bad MAC, and replays with `seq` ≤ the
last seen for that `boot_id`, silently (no reply). A retry must therefore
use a fresh `seq`. Replies echo `boot_id` and `seq` and are MAC'd with the
same key.

### Types

`0x01 CheckIn` (gateway → server). Body:
`t1_unix_ms: i64` (gateway clock at send), `gw_fw: u32`, `batt_mv: u16`,
`rsrp_dbm: i16`, `uptime_s: u32`, `flags: u16` (bit 0 gateway time valid,
bit 1 board link ok), `board_frames_len: u16`, then `board_frames`: the
board's latest `Hello`, `Status` and `FieldStatus` as decoded v1 frames
(header + payload + CRC, no COBS), concatenated.

`0x81 CheckInReply` (server → gateway). Body:
`t1_unix_ms: i64` (echo), `t2_unix_ms: i64` (server receive),
`t3_unix_ms: i64` (server send), `config_version: u32`, `schedule: [u8; 20]`
(`SetSchedule` bytes `enabled`..`bin_ms`), `lease_s: u32` (lease length the
gateway should grant, from now), `wake_interval_s: u32` (gateway check-in
period), `request_flags: u16` (bit 0 send pending windows, bit 1 reboot
gateway), `reserved: u16`.

Time: the gateway stamps `t4` on receive. Offset = `((t2 − t1) + (t3 −
t4)) / 2`, round trip = `(t4 − t1) − (t3 − t2)`. Use only exchanges with a
round trip < 3 s; keep the lowest-RTT sample of the session.

`0x02 WindowPart` (gateway → server). Body: `board_id: u64`, `run_id: u32`,
`part: u16`, `part_count: u16`, `total_len: u32`, `data_len: u16`, `data`.
The reassembled byte string is the `WindowInfo` frame (decoded v1 frame,
102 B) followed by the window data bytes. Parts carry ≤ 1100 data bytes.

`0x82 WindowAck` (server → gateway). Body: `board_id: u64`, `run_id: u32`,
`complete: u8`, `reserved: u8`, `part_count: u16`, `received_bitmap:
[u8; 32]` (bit `i` set = part `i` stored; LSB-first, so part `i` is
bit `i % 8` of byte `i / 8`). The gateway resends missing
parts. After `complete = 1` it sends `AckWindow` to the board.

`0x03 Event` (gateway → server). Body: `t_unix_ms: i64`, `code: u16`,
`len: u16`, `text: [u8; len]` (UTF-8, ≤ 256). Codes: 1 boot, 2 board link
lost, 3 board link restored, 4 RTC step applied (text has old/new), 5 modem
error, 6 window dropped. `0x83 EventAck`: `t_unix_ms` echo.

### Gateway cycle

1. Wake (timer). Drive P0.31 high first thing at every boot.
2. Wake the board (`0x00`), `Query` + `GetFieldStatus`.
3. Attach LTE-M if not attached (PSM requested, eDRX off). `CheckIn`; apply
   the reply's time; push `SetSchedule` to the board if `schedule_crc`
   differs or a lease is due; `SetRtc` if > 250 ms off.
4. For each pending window: `GetWindow` header + chunks, verify
   `data_crc32`, send `WindowPart`s, on `complete` send `AckWindow`.
5. Sleep until the next of: `wake_interval_s` or 60 s after the board's
   next window ends (`next_pulse_unix_s + recovery_s + 60`).

If the server is unreachable the gateway still syncs nothing, leaves the
board's schedule alone and retries next wake. Windows stay on the board.

## 3. Defaults for the first deployment

- Pulse 1000 ms every 3600 s, phase 0, baseline 60 s, recovery 180 s,
  25 Hz, 500 ms bins. SFK1 (`flags` bit 1, plus each probe's kind) once the
  server decodes it.
- Gateway check-in every 3600 s plus the post-window wake.
- Uplink ≈ 12.6 kB per binned pulse window (about 0.4 kB with SFK1) plus
  about 0.3 kB per check-in.
