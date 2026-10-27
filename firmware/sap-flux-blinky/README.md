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

## Heater monitor (firmware 0.4.0)

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

  The board streams from boot and for 10 min after each host frame. After
  that it replies only and sleeps between runs exactly like the field link.
  Any bus traffic wakes it at once (the SP3485 receiver stays on): the host
  sends `0x00`, waits ≥ 10 ms, then a command, and streaming resumes.
  `sap-flux-monitor` does this on its own and sends a `Query` every minute
  while connected, so an open monitor keeps it streaming. SWD keeps working
  while it sleeps (DBGMCU DBG_STOP keeps the clocks running in Stop), so
  measure sleep current with the field strap.
- **PA6 to ground before reset: field.** LPUART2 on J6 replies only:
  - J6 p2 / PA13 is RX, from the Feather TX through 1 kΩ.
  - J6 p3 / PA14 is TX, to the Feather RX through 1 kΩ.
  - J6 p4 is ground. Do not connect J6 p1 / 3.3 V.

  Nothing is sent unsolicited. PA14 stays analog between replies, and each
  reply burst starts with `0x00` after a 200 µs idle-high. The SP3485
  receiver is disabled (~RE high, DE low) and USART2 is not configured. The
  SP3485 has no shutdown mode, so it may still draw ~0.8 mA; measure it. Commands that
  arrive during the heater pulse are answered after it. There is no hot
  switching. Before attaching an ST-Link, open PA6, reset, and disconnect the
  Feather TX.

  Between runs the field board does not measure. It takes frames only during
  a run (baseline, pulse, recovery) and for 30 s before each scheduled
  baseline, so the thermistors have warmed under excitation. Otherwise:
  - Excitation is low, the muxes are disabled and the internal ADC is off.
    The ADS1115s already power down between single-shot conversions.
  - The core sits in Stop 1 with the internal and SPI flash powered down.
  - It wakes every 1.5 s to pet both watchdogs, read the battery voltage for
    FieldStatus and run the scheduler.

  A falling edge on PA13 (the host's `0x00` wake byte) also wakes it. The
  board then stays out of Stop for 1 s after its last command.

### Autonomous schedule and windows

`SetSchedule` validates the schedule, stores it in EEPROM and applies it.
`GetFieldStatus` and `Query` report it. Scheduled runs go through the same
heater state machine as host commands; there is no safety bypass. With the
RTC invalid, a run starts one interval after boot or after the schedule
change, and its window is flagged `TIME_INVALID`.

With the schedule's SFK1 flag, each probe with a kind is encoded as one SFK1
record instead, using the profile its kind and preset name. This firmware
links:

| kind | preset | profile | accurate to |
|---|---|---|---|
| East30 | `12` | `East30K12` | 70 cm/h |
| East30 | `20-mixed` | `East30K20Mixed` | 165 cm/h |
| East30 offset | `12` | `East30Offset` | 70 cm/h |

`SetSchedule` refuses any other preset with `INVALID_PARAMETER`. Each preset
costs flash; the list is `linked!` in `sap-flux-core/src/sfk1win.rs`. A
schedule stored by firmware before presets (20-byte schedule) fails its
record check at boot, and the factory default replaces it.

Each binned run records one window: 500 ms bins of 13 channels plus v24, in RAM
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
   - Between runs, board 3.3 V current should be tens of µA, with
     excitation (PC6) low. It should rise only from 30 s before each
     baseline until the end of recovery.
3. `sap-flux-field set-schedule` with a short interval:
   - Confirm a window appears and `get-window` reconstructs it.
   - Confirm `ack-window` frees it.
4. Power-cycle and confirm that the schedule, the boot count (+1) and the
   trim persist.
5. Bench strap: confirm `sap-flux-monitor` still streams Sample12. Close
   it, wait 10 min, and confirm the bus goes quiet; reopen it and confirm
   streaming resumes within a few seconds.
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
10. SFK1 windows: `sap-flux-field set-schedule --sfk1 --probe1 east30`
    (plus `--probe2` if a second probe is fitted; repeat with
    `--preset1 20-mixed`, and confirm `--preset1 16` is refused). After a run, confirm
    `get-window` lists one record per probe and writes the frame CSV. Then
    `get-raw` the same run and confirm the decoded curves track the raw
    frames within a few counts, including the first seconds after the pulse.

There is one build, the production one (600 s cooldown), for the bench and
the field alike:

```sh
cargo build --release --bin heater_monitor
```

Flash through the ST-Link:

```sh
cargo run --release --bin heater_monitor
```

Reset the MCU after flashing:

```sh
probe-rs reset --chip STM32G0B1CB --protocol swd
```
