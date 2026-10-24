# Sapflux nRF9160 gateway

Rust/Embassy firmware for the CircuitDojo nRF9160 Feather (micro-USB
generation, v1–v3). The Feather is the field gateway for the sap-flux board.
It talks to the board over the J6 TTL UART and to the sapflux server over
LTE-M/UDP (Hologram SIM). The protocol contract is
[`docs/field-protocol.md`](../../docs/field-protocol.md). Power and time
design notes are in [`docs/field-gateway-design.md`](../../docs/field-gateway-design.md).

The image is a known-working NCS 2.1.2 TF-M secure image (`0x10200..0x1c200`)
plus this Rust application, which runs non-secure from `0x1c200`. Nordic's
modem library requires that split. The bootloader is the Feather's
factory-era MCUboot, and you flash the image through its serial recovery.

## Binaries

| binary | what |
|---|---|
| `sapflux-nrf9160-gateway` (default) | Field gateway (below) |
| `diag` | LTE-M/Hologram long-run diagnostic: modem, SIM and registration report at 1 Mbaud. It never opens a socket. |

## What the gateway does

Each cycle follows the spec's "Gateway cycle":

1. Every boot drives P0.31 (NRF_PS_EN) high before doing anything else.
2. It wakes the board with `0x00`, waits 12 ms, then sends `Query` and
   `GetFieldStatus`. Each command is retried up to 3 times, with per-attempt
   timeouts of 200/300/600/1700 ms; the last one rides out a heater pulse.
3. It attaches to LTE-M if needed (PSM requested, eDRX off) and sends a
   `CheckIn` with the board's Hello/Status/FieldStatus frames. Time sync uses
   four timestamps, keeping the lowest RTT and rejecting RTT ≥ 3 s. It pushes
   `SetSchedule` if `schedule_crc` differs or the schedule needs a lease (the
   lease is renewed every check-in). It sends `SetRtc` only if the board is off
   by more than 250 ms and is in SafeIdle or Cooldown.
4. If the server asks for windows (`request_flags` bit 0), each pending window
   is fetched (header plus 100-byte chunks), checked against `data_crc32`, and
   uploaded as `WindowPart`s (up to 5 resend rounds). An `AckWindow` goes to
   the board once the server reports `complete`.
5. It sends queued events (boot, board link lost/restored, RTC step, modem
   error, window dropped) and honours a reboot request.
6. It sleeps until the sooner of `wake_interval_s` (default 3600 s) and
   `next_pulse_unix_s + recovery_s + 60` (board clock). The minimum sleep is
   30 s. While the server or network is failing, the sleep backs off
   120 → 240 → 480 → 960 → 1920 s.

Robustness:

- The nRF WDT (40 s, keeps running in sleep) is petted only while the main
  loop keeps checking in.
- A panic stores its message in retained RAM, then lets the WDT reset the
  chip (TF-M blocks SYSRESETREQ from non-secure). The message is reported in
  the next boot event.
- `boot_id` is a SHA-256 over timer jitter, modem AT responses, uninitialized
  RAM and a retained boot counter. The CryptoCell RNG is secure-only under
  this TF-M build, so this is *not* a CSPRNG. It only has to differ per boot.

## Configuration (server address and key)

The server IPv4 literal, UDP port, device id override and 32-byte HMAC key
are compiled in from `gateway.local.toml`. That file is gitignored and must
never be committed.

```sh
cp gateway.example.toml gateway.local.toml
$EDITOR gateway.local.toml
```

- `server_ip`: the server's public IPv4 address. No DNS lookup is done.
- `server_port`: the server's `udp_bind` port (default 47800).
- `device_id`: `0` uses the modem IMEI (`AT+CGSN`, the 15 digits as a u64).
  The server's `[[device]] device_id` must equal it.
- `key`: 64 hex digits. It must be identical to the server's `[[device]]
  key`. `sapflux keygen --name box1 --device-id <IMEI>` prints both halves;
  copy its `SAPFLUX_KEY_HEX` value into `key` and its `SAPFLUX_DEVICE_ID`
  value into `device_id` (or leave `device_id` at 0).

`SAPFLUX_GATEWAY_CONFIG=/path/to/file.toml` selects a different file.
Without any local file, the build falls back to the example and prints a
cargo warning. The firmware then logs `WARNING: built with
gateway.example.toml`, and the server rejects its MACs.

## Build and package

```sh
cargo build --release                       # ELF only
tools/package.sh                            # build + signed build/app_update.bin
tools/package.sh --features log-always-on   # keep the log UART on while sleeping
```

`tools/package.sh` does the following:

1. Converts the ELF to Intel HEX.
2. Merges it with the TF-M `tfm_s.hex` (`tools/merge_tfm.py`, run under the
   NCS toolchain python, which has intelhex).
3. Writes a flat binary from `0x10200`.
4. Signs it with `imgtool` using the MCUboot `root-ec-p256.pem` key, a 0x200
   header and slot size 0x78000.

The inputs default to the NCS v2.1.2 install under `/opt/nordic` and to
`build/modem-dfu-server-ncs212/tfm/bin/tfm_s.hex`. Override them with
`TFM_HEX`, `NCS` and `NCS_TOOLS`. The output is `build/app_update.bin`
plus a versioned copy.

Never flash the raw ELF, the Rust HEX or the unsigned merged binary through
MCUboot.

Features:

- `log-always-on`: keeps UARTE0 enabled between cycles. By default it is
  released so the TX pin does not back-power the CP2102N on battery.
- `latch-sleep`: experimental and not verified. Instead of sleeping on the
  timer, it sets a PCF85063A alarm and drops P0.31 so the regulator turns
  off; every wake is then a cold boot. Only enable it after the bench test
  below.

## Flashing (MCUboot serial recovery)

1. Hold MODE (P0.12), tap RESET, and release MODE. MCUboot waits in serial
   recovery on the USB UART.
2. Upload the image, then reset:

   ```sh
   PORT=/dev/cu.usbserial-XXXX   # the CP2102N; check ls /dev/cu.*
   ~/go/bin/mcumgr --conntype serial --connstring "dev=$PORT,baud=1000000" image upload build/app_update.bin
   ~/go/bin/mcumgr --conntype serial --connstring "dev=$PORT,baud=1000000" reset
   ```

## Watching logs

The log is on the CP2102N (UARTE0 TX = P0.06) at **1 Mbaud**, 8N1, one line
per event with an uptime prefix:

```sh
screen /dev/cu.usbserial-XXXX 1000000      # or: picocom -b 1000000 ...
```

By default the log UART is released between cycles, so nothing prints while
the gateway sleeps. Build with `--features log-always-on` for bench work.

## Wiring

| Feather | Board J6 | notes |
|---|---|---|
| P0.24 (TX) | p2 (board RX, PA13) via 1 kΩ | held high between cycles |
| P0.23 (RX) | p3 (board TX, PA14) via 1 kΩ | no pull (PA14 has the BOOT0 pull-down) |
| GND | p4 | |

Other pins used: P0.31 NRF_PS_EN (driven high), P0.20/AIN7 battery divider
enabled by P0.25, TWIM1 P0.26/P0.27 (LIS2DH12 0x18, PCF85063A 0x51), and the
W25Q32 flash (CS P0.07, SCK P0.11, MOSI P0.09, WP P0.08, HOLD P0.10), which
is put into deep power-down at boot. These pins come from the NCS v2.1.2
`circuitdojo_feather_nrf9160` board files; check them on your board revision.

## Bench tests (before the field)

Run these with `--features log-always-on`, on battery where noted.

1. **Boot and config.** The log shows the version, the reset reason, the
   IMEI-derived `device_id`, `boot_id`, and no `WARNING` about the example
   config. Two resets give different `boot_id`s.
2. **Board link.** With the board in field mode (PA6 low), `board <id>:
   pending … next_pulse …` appears, and the end-of-cycle counters show 0
   failures. Unplug J6: a `board link lost` event is queued. Replug: `link
   restored` on the next cycle.
3. **Commands during a pulse.** Start a cycle while the heater is on.
   Commands must still succeed (the 1700 ms attempt), with no SetRtc during
   the window.
4. **Check-in and time.** Run the server, or `sapflux-gateway-sim` to compare
   behaviour. Expect `checkin reply: offset … rtt … (applied)` and an RTT
   well under 3 s. Set the board RTC 5 s off: one `SetRtc` and an RTC step
   event follow, and the next cycle logs `board rtc ok`.
5. **Network time fallback.** Point the gateway at an unreachable server.
   `clock: network time …` should appear. Check that the `AT+CCLK?`
   time-zone sign handling gives correct UTC (compare with a known clock).
6. **Schedule push.** Change the schedule on the server (`sapflux schedule`).
   The next cycle logs `schedule pushed (crc …)`, and the board's
   `schedule_crc` matches afterwards. Check that an all-zero or invalid
   server schedule is not pushed.
7. **Windows.** After a pulse, with `send_windows` on, expect `window N: …
   fetched, uploading`, then `delivered and freed`, and the board's
   `windows_pending` drops. Corrupt a chunk (or make the server drop parts)
   to exercise the resend rounds.
8. **PSM and current.** Confirm the network grants PSM: the CEREG reply logged
   at attach carries the granted T3412/T3324. Measure sleep current on
   battery with the log UART released. Check that the LIS2DH12 power-down
   and flash deep power-down log lines appear, and measure what they save.
9. **Battery reading.** The logged `battery … mV` should match a meter within
   about 50 mV. This confirms P0.25 is an active-high divider enable on this
   revision.
10. **Watchdog.** Temporarily put a `loop {}` in the cycle: the chip must reset
    within about 3 minutes (the grace period plus 40 s), and the next boot
    reports reset `watchdog`. A forced panic must appear in the next boot
    event.
11. **Server reboot request.** Setting the reboot flag on the server makes the
    gateway reset within 40 s of the cycle finishing.
12. **latch-sleep (only if you want it).** On battery, check that dropping
    P0.31 really powers the Feather off and that the PCF85063A alarm brings
    it back. If the board keeps running, the firmware falls back to the timer
    sleep and logs `still powered`.
