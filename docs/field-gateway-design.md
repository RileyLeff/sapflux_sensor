# Field gateway design (v0.2 board + nRF9160 Feather)

Status: draft, 2026-10-24. Covers the first unattended field deployment:
two sap-flux boards, each with an East30-style probe, reporting over LTE-M.

## Topology

```
probe A ──25 m── board A ─┐ J6 TTL UART           ┌─ Hetzner VPS (public IP, UDP ingest)
                          ├── nRF9160 Feather ──LTE-M──┤
probe B ──25 m── board B ─┘ J6 TTL UART           └──tailnet── acacia (long-term store)
```

Proposed: both boards and one Feather share one enclosure. The nRF9160 has
four SERIAL instances with fully remappable pins. UARTE0 stays on the USB
console, and UARTE1 and UARTE2 each serve one board, so the two boards need
one SIM, one modem and one battery. If the boards end up in separate boxes
more than a couple of metres apart, use one Feather per board instead. TTL
UART is not for long cables.

## Board ↔ gateway link

The J4 pins don't have a UART. On the STM32G0B1, PA6 and PA7 offer only
SPI1, timer channels, USART3/6 CTS/RTS and I2C2/I2C3 (AF8/AF9). The only
spare UART pins on a header are J6:

| option | pins | verdict |
|---|---|---|
| **LPUART on J6** | PA13 = RX, PA14 = TX, PA6 strap selects at boot | **use this**: already implemented in `heater_monitor`, full duplex, and LPUART can wake the MCU from Stop |
| RS-485 + transceiver on Feather | J5 | works over long cables, but needs the bus receiver powered or polled and the half-duplex command windows |
| I2C target on J4 SDA/SCL | I2C1 | shares the bus with the ADS1115 and EEPROM, so it would need multi-master; no |
| I2C target on PA6/PA7 (I2C2/3) | J4 p5/p6 | possible, but more firmware and pull-up work, and it removes the PA6 strap |

Caveats for J6:

1. **The peripheral name needs checking on the bench.** Embassy's pin database
   (from STM32Cube) maps PA13/PA14 AF10 to **LPUART2**. Datasheet DS13560
   rev 1, table 14, says **LPUART1**. `heater_monitor` uses LPUART2. Before
   relying on it, scope PA14 or loop it back while the board sends Hello.
2. **The BOOT0 pull-down is on PA14.** On LQFP-48 BOOT0 shares the pin with
   PA14, and the 10 kΩ `r_boot0` pulls it down. An idle-high TX pin therefore
   costs 330 µA. In field mode, drive TX only while transmitting and leave the
   pin as analog/input otherwise. The Feather's RX must not enable its own
   pull-up: with the pull-down it would make a mid-rail divider.
3. **No SWD in field mode.** Grounding PA6 at reset gives J6 to the UART. To
   reflash, remove the strap and disconnect the Feather's TX first. Never
   connect J6 pin 1 (3.3 V) between the boards. Wiring is J6 p2 (board RX) ←
   1 kΩ ← Feather TX, J6 p3 (board TX) → 1 kΩ → Feather RX, and J6 p4 to
   GND.
4. **Make the link pull-based.** Today the board streams 50 Hz Samples on
   every transport (about 2.7 kB/s), which keeps the Feather's UARTE and
   HFCLK running. In the field the gateway should wake, send one 0x00
   preamble byte so the LPUART can wake the MCU from Stop, then ask for
   status and stored records. The board answers and both go back to sleep.

## Who runs the schedule, and the remote off switch

Proposed: **the board runs the pulse schedule, and permission to heat comes
from a lease the gateway renews.**

- The board already owns heater safety: the pulse state machine, EEPROM
  lockout, cooldown, TPL5010 and the SCR latch. The scheduler adds one more
  input: `auto_enabled_until` (RTC time).
- The server holds `{enabled, interval, offset, pulse_ms, baseline_s,
  recovery_s}` per board. Every uplink returns the current config. The
  gateway forwards it and, if `enabled`, renews the lease (for example
  `now + 6 h`).
- If you turn it off on the server, it stops at the next uplink. If the
  cellular link, gateway or server dies, the board stops pulsing by itself
  when the lease runs out. Losing comms fails safe.
- Data keeps logging to the board's 32 MB flash whether or not the gateway
  is up.
- Stagger the two boards (for example :00 and :15) so the two 14.4 W heaters
  never overlap on the 24 V supply and neither pulse lands in the other's
  measurement window.

The simpler alternative is to have the gateway send StartBaseline/Arm/Fire
using the existing protocol. That needs no new board firmware, but the
Feather has to stay awake and receiving for the whole of every pulse window,
and a hung gateway in the middle of a sequence leaves an Armed board, which
the board then has to handle.

## Time

What accuracy is needed:

- **Within a pulse (seconds to minutes): the time base must be
  crystal-accurate.** τ, K and V scale directly with any error in the sample
  clock.
- **Absolute time:** about ±1 s is plenty to line up the two sensors and to
  match meteorological data.

Current state:

- `monotonic_us` comes from TIM2 clocked by **HSI16**, which is
  −0.75…+0.5 % at 30 °C plus up to ±1 % drift over 0–85 °C (DS13560
  table 44). Left uncorrected, that is a percent-level error in the
  velocity estimate.
- The RTC runs from the LSE crystal (±20 ppm, about 1.7 s/day, plus about
  −14 ppm at ±20 °C from 25 °C because of the tuning-fork curve). A CR2032
  on VBAT through the BAT54C keeps it running through 24 V outages.
- Each Sample already carries both `monotonic_us` and `rtc_unix_ms`. The RTC
  subsecond counter has 3.9 ms resolution, so a regression over one 240 s
  window recovers the HSI/LSE ratio to well under 1 ppm. **Keep
  `rtc_unix_ms` in stored records** (at least at window start and end) and
  rescale time in post-processing. Later, the firmware can calibrate HSI
  against LSE with TIM16/17 input capture.

Plan:

1. **Gateway time comes from the server**, NTP-style, in the same UDP
   exchange as each uplink. The gateway records its send and receive times,
   the server stamps its receive and send times, and the four timestamps
   give the offset with error bounded by the round-trip time (typically
   0.1–1 s on LTE-M). There's no extra traffic, and outlier RTTs can be
   thrown away. Network time (`AT%XTIME` / `AT+CCLK?`) is the fallback. It
   has 1 s resolution and depends on the carrier.
2. **Gateway → board:** use `SetRtc` only when the error exceeds about
   0.5 s, and never during a run (it is already restricted to SafeIdle).
   Log every correction with its before/after offset so the drift is
   documented.
3. **Discipline the board RTC:** once a few days of corrections exist, set
   RTC_CALR smooth calibration (0.95 ppm steps) from the measured drift.
4. GNSS on the nRF9160 can't run while LTE is active and costs tens of mA
   for 30 s or more per fix. Not needed for ±1 s.

## Data tiers

Measured on the Aug 24 soaked-4×4 run in
`analysis/outputs/compression_20261024/`. It is one pulse, so recheck on the
first field pulses.

| tier | where | size per pulse | notes |
|---|---|---:|---|
| lossless 50 Hz window (60 s + 180 s) | board flash, always | about 46 kB | mean-of-8 predictor + Rice, about 4.5× over packed 18 B records |
| parameters + 3-number baseline + 12-knot log-time spline per channel | uplink, every pulse | about 380 B | every fitted parameter within 1σ, 0.7 mK RMS |
| 1-minute idle summaries | flash + hourly uplink | 30 B/min | 50 Hz idle doesn't fit, even lossless |
| raw window by run ID | uplink, on request | about 46 kB | for flagged pulses or spot checks |

- **Flash:** 32 MiB per board holds lossless windows plus idle summaries for
  about 29 days at 24 pulses/day and about 15 days at 48 pulses/day.
- **Uplink, both boards:** about 3.5–4 MB/month by default.
- **Decimation:** uniform averaging down to 2 Hz is safe; 1 Hz is borderline.
  With uneven bins, the fit must weight each bin by its width, or τ moves
  35–63σ.
- **Model alone isn't enough:** the Marshall model's residual is about 2.3×
  the noise. Keep the model parameters as the result, and keep the spline
  and raw data as the archive.

## Server

The Feather on Hologram can't reach tailnet addresses (100.x), so it needs
a public endpoint. Proposed: a small Hetzner VPS with a public IPv4 running a
Rust UDP ingest (reusing `sap-flux-protocol`), with each datagram
authenticated by a per-device key (HMAC-SHA256 truncated to 8 bytes). The VPS
joins the tailnet and replicates to acacia. Using the IP literal avoids DNS
lookups. Home port-forwarding to acacia also works, but brings a dynamic IP,
possible CGNAT and exposure of the home network. Tailscale Funnel is HTTPS
only, so every connection would pay for a TLS handshake.

## Power

### Board (24 V system)

- Field mode samples at a low idle rate (for example one frame every 10 s or
  every minute) with the MCU in Stop in between, and runs 50 Hz only during
  the baseline/recovery windows.
- Excitation stays on only during conversions. Today it is held high
  continuously.
- RS-485: hold ~RE high and DE low in field mode, which disables the
  receiver (800 µA typ). The SP3485 datasheet doesn't specify a shutdown
  current, so measure it. Re-enable RS-485 only with a bench strap or
  command.
- J6 TX is driven only while sending (see the BOOT0 caveat above).

### Gateway (Feather v1–v3, micro-USB; findings from the v1/v2 Eagle schematics)

- **Firmware must drive P0.31 (NRF_PS_EN) high at boot, or the board turns
  itself off.** On battery alone the TPS63031 enable is a diode-OR of VBUS,
  the MODE button, the PCF85063A RTC alarm and P0.31. Zephyr does this
  automatically; the Rust firmware currently does not. On USB it works
  anyway, so the bench never shows the problem.
- **PSM isn't a low-power state on this revision.** The TPS63031 PS/SYNC pin
  is tied to VSYS, which forces PWM mode. Expect mA-level input current
  whenever the regulator runs (estimated from the datasheet curve; measure
  it). The real sleep is the latch: set a PCF85063A alarm (I2C 0x51), drive
  P0.31 low, and the board drops to under 3 µA until the alarm turns it back
  on. Every wake is therefore a cold boot plus a full LTE-M attach.
- Each wake: attach, sync time, pull records from both boards, upload,
  receive config, set the next alarm, latch off. Plus an immediate small
  alert on any board fault the next time it is awake.
- Drains to cut while awake:
  - LIS2DH12 SDO pull-up, about 130 µA (CTRL_REG0 = 0x90, then ODR = 0).
  - W25Q32 deep power-down (0xB9).
  - Blue LED on P0.03, active low.
  - UART0 to the unpowered CP2102N: leave its pins as disconnected inputs.
- GNSS needs an active antenna on the U.FL plus `AT%XCOEX0`. Not planned.

### Gateway battery: 18650 LiFePO4

- **A LiFePO4 cell on the JST battery input is fine.** The TPS63031
  buck-boost holds 3.3 V down to about 1.8 V input. Peak TX current is about
  0.4–0.6 A from the cell, which is inside its 500 mA boost limit.
- **Never plug USB in with LiFePO4 attached.** The STBC08 charger is always
  on when VBUS is present and charges toward 4.2 V; LiFePO4's maximum is
  3.65 V. Unplug the battery before USB flashing or recovery. To remove the
  hazard permanently, remove R12 (the 3.4 kΩ PROG resistor), which puts the
  charger in shutdown.
- **Paralleling cells:**
  - Use the same model and age.
  - Charge each cell to full (3.65 V CV until the current tapers) on a
    LiFePO4 charger before connecting them. The flat LiFePO4 curve means a
    matching voltage reading doesn't prove a matching state of charge.
  - Check whether the holders are wired in series: two cells reading about
    6.5 V means series. Rewire or buy parallel holders.
  - Join the holders with short, thick leads. Add a per-cell fuse of about
    2 A if you want to be careful.
  - Spring-contact resistance is tolerable at these currents.
- Estimated life on two 1.5 Ah cells (90% usable, latch-off sleep of about
  8 µA total). These are estimates, not measurements:

| wake period | awake | average | life |
|---|---|---:|---:|
| 30 min | 45 s @ 30 mA | 0.76 mA | ~150 days |
| 30 min | 90 s @ 60 mA | 3.0 mA | ~37 days |
| 60 min | 45 s @ 30 mA | 0.38 mA | ~290 days |
| always on, forced-PWM regulator ~5 mA | — | 5 mA | ~22 days |

## Bench checklist

- [ ] Confirm whether PA13/PA14 are LPUART1 or LPUART2 (scope PA14 in UART mode).
- [ ] Measure board current: streaming vs. RS-485 receiver off vs. Stop.
- [ ] Identify Feather revision (micro-USB = v1–v3; LIS2DH12 present = v2+).
- [ ] Verify P0.31-high keeps the Feather on with USB unplugged; verify latch-off + RTC-alarm wake.
- [ ] Measure Feather current: latched off, awake idle (forced PWM), and energy per wake/attach/upload.
- [ ] Measure SP3485 current with ~RE high and DE low.
- [ ] Confirm the CR2032 is fitted and the RTC survives removal of 24 V.
- [ ] Measure clock ratio: regress `rtc_unix_ms` against `monotonic_us` on an existing capture.
