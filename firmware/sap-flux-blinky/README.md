# SAP Flux Blinky

Minimal Embassy firmware for the SAP Flux Adapter board.

The onboard RGB LED is common-anode. The STM32 sinks the LED cathodes, so the GPIO logic is inverted:

- `PB3` low: red on
- `PB4` low: green on
- `PB5` low: blue on

On the v0.1 assembled boards, the LED appears to be rotated 180 degrees:

- `PB5` / board blue net behaves as the accidental common anode
- `PB4` / board green net lights the red die
- `PB3` / board red net lights the blue die
- green is not firmware-recoverable because its cathode appears to land on `V3V3`

That note is historical: v0.2 has the corrected component orientations.

## Time base

All binaries use `embassy-time` at 32 768 Hz on LPTIM1 clocked from the
32.768 kHz LSE crystal (`src/timebase.rs`), so sample timing and
`monotonic_us` are crystal-locked (30.5 µs ticks). Every binary must call
`sap_flux_blinky::init(config)` instead of `embassy_stm32::init`, or time never
advances. embassy-stm32 0.6's own `time-driver-lptim1` does not build for
STM32G0. If the LSE does not start, the driver falls back to the LSI (time then
runs about 2.3 % slow); the heater pulse width is timed by TIM1 on HSI16 either
way.

## Heater monitor (firmware 0.3.0)

`heater_monitor` samples both probes (12 thermistors + excitation) every 40 ms
and implements `docs/field-protocol.md` §1 on top of protocol v1.

Thermistor order in `Sample12` and windows is probe 1 T1..T6, then probe 2
T1..T6. Probe 1 is J2 block A (mux A Y1, Y0, Y3, Y2, Y5, Y4, bench-verified).
Probe 2 is J2 block B (p23..p28) on mux B with the same channel order, taken
from `elec/adc.ato` and **not yet read on hardware**.

### Link selection (J4 pin 5 / PA6, sampled once at boot)

- **PA6 open: bench.** v1 over RS-485 at 115200 8N1. One `Sample12` per frame,
  `Status` once a second, Hello + Status at boot. The last frame of each
  1 s cycle carries `FLAG_COMMAND_WINDOW` and the next frame is silent
  (about 45 ms quiet for one command). The legacy `Sample` is no longer sent.
- **PA6 to ground before reset: field.** LPUART2 on J6 replies only:
  - J6 p2 / PA13 is RX, from the Feather TX through 1 kΩ.
  - J6 p3 / PA14 is TX, to the Feather RX through 1 kΩ.
  - J6 p4 is ground. Do not connect J6 p1 / 3.3 V.

  Nothing is sent unsolicited. PA14 stays analog between replies, and each
  reply burst starts with `0x00` after a 200 µs idle-high. The SP3485 is held
  in shutdown (~RE high, DE low) and USART2 is not configured. Commands that
  arrive during the heater pulse are answered after it. There is no hot
  switching. Before attaching an ST-Link, open PA6, reset, and disconnect the
  Feather TX.

### Autonomous schedule and windows

`SetSchedule` validates the schedule, stores it in EEPROM and applies it.
`GetFieldStatus` and `Query` report it. Scheduled runs go through the same
heater state machine as host commands; there is no safety bypass. With the
RTC invalid, a run starts one interval after boot or after the schedule
change, and its window is flagged `TIME_INVALID`.

Each run records one window: 500 ms bins of 13 channels plus v24, in RAM
(4 windows of up to 1000 bins, 104 000 B; longer windows are flagged
`TRUNCATED`). A finished window is then written to the next of 256 W25Q256
slots (32 KiB each, upper 8 MiB), read back against RAM, and only then freed
from RAM, so a reset loses no undelivered window. `GetWindow` serves RAM
windows directly and flash windows after waiting at most 150 ms for the chip;
`AckWindow` marks the slot delivered (marker byte programmed shortly after).
256 slots hold 10.7 days of undelivered windows at 24 pulses/day (5.3 at 48);
past that a new window overwrites the oldest and `windows_dropped` counts it.
A dead chip (FieldStatus `flash_ok` clear) falls back to the 4 RAM windows.

### Flash (W25Q256JV on SPI2)

At boot the JEDEC ID must read `EF 40 19`; otherwise `flash_ok` is clear and
the bench link sends an `Error` with fault code `0x0100` (detail = ID bytes
2..3). Every frame from Baseline through Recovery (scheduled or
host-commanded) is appended to a raw ring in the lower 24 MiB: 256-byte
blocks of up to 12 delta-coded frames with run id, sequence number and
CRC-32, each run starting on a fresh 4 KiB sector. A default 241 s window is
about 503 blocks (128 KiB with alignment), so the ring keeps about 191 runs:
8.0 days at 24 pulses/day, 4.0 at 48. Boot scans the 6144 sector headers and
256 slot headers in about 0.4 s. Erases run ahead of the write head and never
while the heater is on; a block that cannot be queued is dropped rather than
delaying anything. `FieldStatus.flash_used_kib` is the raw ring plus the
window slots in use. `sap-flux-field get-raw --run <id>` downloads a run
(`GetRaw`), checks every block CRC and writes `raw_<id>.csv` and `.bin`.

### Time

`SetRtc` is accepted in SafeIdle and in Cooldown, never while a window
records. It still steps on the next whole second, so its reply can take up
to 1 s. A retried nonce is acknowledged without stepping again. Corrections
at least 6 h apart update RTC_CALR (α = 0.5, ±200 ppm). The learning state is
kept in TAMP bkpr3/4 and the trim in EEPROM; the trim is restored at boot.

### EEPROM (CAT24C32) and watchdogs

EEPROM layout:

| address | record |
|---|---|
| `0x0000` | schedule |
| `0x0020` | RTC trim |
| `0x0040` | boot count |
| `0x0FC0` | heater lockout (unchanged) |
| `0x0FF0` | `board_test` scratch |

Each record is magic + payload + CRC-32, all within one page.

The IWDG (4 s timeout) runs alongside the TPL5010 external watchdog.

The lockout record is read at boot up to 3 times, 10 ms apart. If it still
cannot be read (or a later lockout write fails), the heater stays locked
out, the schedule is held, and FieldStatus shows `eeprom_fault` (bit 6).

### Bench checks before field use

1. Probe 2 order: touch each probe-2 thermistor and confirm that P2T1..P2T6
   respond in order (also confirm probe 1).
2. Field strap:
   - Confirm `sap-flux-field query` works.
   - Confirm there is no traffic between commands.
   - Measure current with the Feather RX idle (PA14 should draw no 330 µA).
3. `sap-flux-field set-schedule` with a short interval:
   - Confirm a window appears and `get-window` reconstructs it.
   - Confirm `ack-window` frees it.
4. Power-cycle and confirm that the schedule, the boot count (+1) and the
   trim persist.
5. Bench strap: confirm `sap-flux-monitor` still streams Sample12.
6. Flash: `sap-flux-field status` shows `flash_ok` and a nonzero
   `flash_used_kib` after one run (with the bench strap, no `Error`
   0x0100 at boot).
7. Raw log: after a run, `sap-flux-field get-raw --run <last_run_id>`
   reports every block CRC verified; check that the CSV has ~25 rows per
   second from Baseline to the end of Recovery, with no gaps around the
   pulse.
8. Windows survive a reset: let a run finish, wait a few seconds, reset the
   board, and confirm that `status` still shows it pending and `get-window`
   reconstructs it (data_crc32 verified). `ack-window`, reset again, and
   confirm it stays delivered (`windows_pending` drops).
9. EEPROM fault (spare board, EEPROM not fitted): confirm `status` shows
   `eeprom_fault|heater_lockout` right after boot and that no run starts.

Build the field firmware:

```sh
cargo build --release --bin heater_monitor
```

The supervised bench build retains the intentionally short 30-second
persistent cooldown and advertises firmware version 0.3.32:

```sh
cargo build --release --bin heater_monitor --features dev-short-cooldown
```

Flash through the ST-Link:

```sh
cargo run --release --bin heater_monitor
```

Reset the MCU after flashing:

```sh
probe-rs reset --chip STM32G0B1CB --protocol swd
```
