# sap-flux-field-cli

`sap-flux-field`: a bench CLI for the board's field mode
(`docs/field-protocol.md` §1) without the nRF9160 gateway. It speaks the same
commands the gateway will: Query, GetFieldStatus, SetSchedule, SetRtc,
GetWindow and AckWindow. Wire code comes from
[`sap-flux-protocol`](../sap-flux-protocol).

Each command follows the gateway's link rules: a single `0x00` wake byte, a
15 ms pause (`--wake-ms`, minimum 10), the command frame, then a wait for the
replies (`--timeout-ms`, default 1800 ms because the board holds replies up
to 1.5 s while the heater is on). A command whose reply never arrives is
re-sent with the same nonce, up to `--retries` (default 3) more times.
SetRtc is re-stamped from the host clock on every attempt. An `Ack` with a
nonzero result is reported and never retried.

## Wiring

- **J6 (field link, LPUART):** a 3.3 V USB-serial adapter at 115200 8N1.
  Adapter TX → J6 p2 (board RX, PA13), adapter RX ← J6 p3 (board TX, PA14),
  GND → J6 p4. Strap PA6 low for field mode.
- **RS-485 (J5):** the FT232R bench adapter with `--rs485`. RTS is held low
  (receive) and raised only while a frame is written. The board has to have
  its RS-485 transport selected; in field mode the SP3485 is otherwise shut
  down.

## Usage

```sh
cd software/sap-flux-field-cli
cargo build --release
alias sff=target/release/sap-flux-field
export SAP_FLUX_PORT=/dev/cu.usbserial-XXXX   # or pass --port each time

sff ports                       # list serial ports
sff query                       # Hello + Status (+ FieldStatus), board-host offset
sff status                      # GetFieldStatus, pretty-printed

# Store a schedule. --enable or --disable is required; every other field
# defaults to the first-deployment values (1000 ms every 3600 s, phase 0,
# baseline 60 s, recovery 180 s, 25 Hz, 500 ms bins).
sff set-schedule --enable
sff set-schedule --enable --interval-s 900 --phase-s 300 --pulse-ms 500
sff set-schedule --enable --require-lease --lease-s 7200
sff set-schedule --disable
# Limits are checked locally first (board cooldown --cooldown-ms 600000);
# --no-validate sends anyway to test the board's INVALID_PARAMETER path.
# After the Ack, the reply's FieldStatus.schedule_crc must match the local one.

sff set-rtc                     # set from the host clock, report offset before/after
sff set-rtc --min-offset-ms 250 # only if off by more than 250 ms (gateway rule)
sff set-rtc --dry-run           # report the offset only

sff get-window --header-only    # WindowInfo of the oldest undelivered window
sff get-window --out-dir captures/windows        # download the oldest window
sff get-window --run 42 --out-dir captures/windows --ack
sff ack-window --run 42         # free it on the board
```

`-v` prints every frame sent and received.

## Window output

`get-window` downloads the header and every chunk (requested by the header's
real run id, so the oldest window cannot change mid-download), checks every
chunk's run id, index and length, and verifies `data_crc32`. It writes nothing
and sends no AckWindow when verification fails. On success it writes:

- `window_<run>.csv`: one row per bin with `bin`, `bin_start_offset_ms`,
  `t_from_heater_on_s`, `bin_start_monotonic_us`, `bin_start_rtc_unix_ms`
  (interpolated between the window's start and end RTC stamps, empty when
  `TIME_INVALID`), then the 13 channels decoded back to raw ADC counts
  (`base_raw + value/16`, 1/16-count resolution): `t1_raw`..`t6_raw`
  (probe 1), `p2t1_raw`..`p2t6_raw` (probe 2), `excitation_raw`. These are
  the monitor's `samples.csv` column names. Missing bins are empty.
- `window_<run>.bin`: the same byte string the gateway uploads in
  `WindowPart`s, a decoded `WindowInfo` frame (102 B) followed by the window
  data. Use it to cross-check the server's reassembly and decoder.

## Tests

```sh
cargo test   # fake-board link/retry/window tests, plus a pty serial round trip
```

No hardware is needed. The serial path is tested over a pseudo-terminal
pair. Nothing here has been run against a real board yet.
