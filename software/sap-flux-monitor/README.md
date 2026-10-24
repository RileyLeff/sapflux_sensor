# sap-flux-monitor

Native macOS desktop monitor for the sap-flux instrument. Connects to the
board over a USB/RS-485 serial adapter (default `/dev/cu.usbserial-BG02Q2RS`,
115200 8N1), shows live thermistor data, and records every received frame to
disk. The wire protocol lives in the sibling
[`sap-flux-protocol`](../sap-flux-protocol) crate (`no_std`, shared with the
STM32 firmware).

## Build and run (macOS)

Requires stable Rust (tested with 1.97). No system dependencies beyond the
Xcode command-line tools.

```sh
cd software/sap-flux-monitor

# run the app (debug)
cargo run

# optimized build
cargo run --release

# tests (this crate; protocol tests live in ../sap-flux-protocol)
cargo test
cargo test --manifest-path ../sap-flux-protocol/Cargo.toml

# lint/format
cargo fmt --check
cargo check --all-targets
```

If the port doesn't appear, check that the USB/RS-485 adapter shows up as
`/dev/cu.usbserial-*` (`ls /dev/cu.*`) and that no other program (e.g.
`screen`, Arduino IDE) holds the port open.

## Using it

The first screen is the instrument monitor. Pick a port (the sap-flux
adapter is pinned to the top of the list and pre-selected when present) and
press **Connect** — or press **Simulator** to run against a built-in
simulated board that produces plausible data, an RTC-valid transition, a
full pulse/recovery/cooldown cycle, six simulated thermistors, and (optionally)
injected CRC/sequence errors. The simulator never opens a serial port and
feeds the exact same decoder/logger/UI path as real hardware.

Channels (labels match the sensor wire colors; T1 is known broken on the
current test probe, so it is struck through and hidden from plots by
default — toggle it back on at any time):

| ch | wire | position |
|----|------|----------|
| T1 | green | downstream tip (known broken on current probe) |
| T2 | yellow | downstream middle |
| T3 | purple | downstream head |
| T4 | orange | upstream tip |
| T5 | white | upstream middle |
| T6 | blue | upstream head |

Nothing mutating is ever sent automatically: connecting only listens. RTC
sync, baseline start, arm, fire, and abort are all explicit buttons, arm/fire
are gated on board-reported state (RTC valid, PGOOD, fresh valid frames,
zero cooldown, matching run in Baseline/Armed), fire requires a confirmation
dialog after a successful arm ack, fire nonces are one-use, and nothing is
retried. The 10-minute post-pulse cooldown countdown shown is board-reported;
the UI never infers permission from its own clock. Abort stops a run at the
earliest safe opportunity but cannot shorten a hardware pulse already in
progress — the firmware safety layers (one-pulse timer, IWDG, TPL5010 + SCR
latch, thermistor interlock) are the real safety boundary.

Heater controls additionally require the board's Hello to advertise the
heater-control capability (`CAP_HEATER_CONTROL`, capabilities bit 3). The
current `heater_monitor` firmware advertises the full capability set (bits
0–3: RTC, ADC, 24 V monitor, and heater control), so the controls become
available only after a fresh Hello and the remaining safety gates pass. A
monitor-only build without bit 3 instead disables Start baseline / Arm /
Fire and shows **"Heater disabled by firmware"**; Set RTC, monitoring,
plotting, and logging still work, and Abort stays available while the board
itself reports an active run state. The simulator also advertises the full
capability set so the whole safety UX remains testable without hardware.

## Half-duplex RS-485: RTS and command windows

The bench adapter is an FTDI FT232R whose RS-485 transmitter is enabled
while **RTS is HIGH**; RTS LOW is receive mode. The link is half-duplex: a
host command written while the board is transmitting collides and is lost.
The monitor therefore follows a strict transmit discipline (from live
bring-up on the real adapter/board):

- **RTS is driven LOW immediately after opening the port** so connecting
  never jams the bus. A failure to do so is surfaced prominently in the UI
  and in `events.csv`.
- The firmware marks deterministic **command windows**: a Sample with flags
  bit 5 (`FLAG_COMMAND_WINDOW`) set means the board stays silent and
  listening for at least ~40 ms after that sample.
- Clicking a command button never writes to the port directly. The command
  is encoded and **queued** (FIFO), the UI shows *"queued for next command
  window"*, and the worker transmits **at most one queued command per
  command window**: RTS HIGH → ~200 µs driver-enable settle → write +
  flush → RTS LOW again (RTS is dropped even if the write fails).
- `CommandSent` is logged only when bytes physically leave the port, never
  when a command is merely queued.
- **Commands are never retried automatically** — not on a missing ack, not
  on a timeout. This is absolute for FirePulse. A queued command is one
  intended transmission, popped exactly once at a command window.
- Disconnect/reconnect **drops every unsent queued command** and clears all
  local armed/fire intent; nothing carries over.

The simulator is exempt: it consumes commands immediately (no windows, no
RTS), since it never touches a serial port.

## Capture files

On the first valid Hello or Sample after connect, a session directory is
created under `captures/` (gitignored):

```
captures/session_YYYYMMDD_HHMMSS/
├── raw.cobs        exact received serial bytes, append-only (replayable)
├── samples.csv     one row per decoded Sample frame
├── events.csv      everything else, with host timestamps
└── metadata.json   session context
```

- **raw.cobs** — the unmodified receive byte stream, including corrupt
  frames. Feeding it back through the protocol decoder reproduces the
  session exactly.
- **samples.csv** — `host_receive_unix_ns, sequence, monotonic_us,
  rtc_unix_ms, run_id, flags` + decoded flag bits, `state`/`state_name`,
  `sample_valid`, `excitation_raw`/`excitation_valid`, `v24_raw`, then per
  channel `tN_raw, tN_res_ohm, tN_temp_c, tN_valid`. Derived fields are
  empty (and `tN_valid=0`) for open/short/nonsense channels instead of
  NaN/inf. Every decoded sample is written — nothing is averaged, decimated,
  or dropped, even when the UI falls behind (plot decimation is
  render-only).
- **events.csv** — `host_unix_ns, kind, detail` rows for Hello/Status/Ack/
  Error frames, connect/disconnect, parser errors, sequence gaps, and every
  outbound command.
- **metadata.json** — app/protocol versions, port settings, board identity,
  channel labels, the exact thermistor conversion (coefficients + formula),
  session start/end, and final counters.

Files are flushed at least once per second, and immediately on status
transitions and outbound commands. Logging happens in the serial worker
before UI delivery, so display backpressure can never lose logged frames
(dropped display events are counted and shown in the Link panel).

## Thermistor conversion

With the excitation rail sampled in the same frame (ratiometric, so raw
counts substitute for volts):

```
R_ntc = 10000 * (exc_raw - mid_raw) / mid_raw     # NTC top, 10 kΩ bottom
l     = ln(R_ntc / 10000)
T_C   = 1 / (0.003354 + 2.565e-4·l + 2.325e-6·l² + 1.024e-7·l³) − 273.15
```

Readings are marked invalid (not NaN) when excitation is dead, the ratio is
nonsensical (`mid ≤ 0` or `mid ≥ exc`), resistance falls outside
200 Ω–500 kΩ, or the temperature falls outside −55…150 °C.

## Hardware status

The RTS behavior, command-window semantics, and capability masks above come
from live bring-up against the real FT232R adapter and board. Everything
else is verified with unit/integration tests and the simulator.
