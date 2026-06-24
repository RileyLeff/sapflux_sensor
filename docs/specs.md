# Sap Flux Sensor Adapter Board — Technical Specifications

## Overview

Open-source adapter board connecting sap flux sensors to a field data logger. Designed for long-duration, unattended forest deployments on 24V solar/battery systems. Firmware built on the Embassy async Rust framework (`no_std`).

## Power

| Parameter | Value |
|---|---|
| Input voltage (continuous) | 5–30V DC |
| Nominal voltage | 24V (12V compatible at reduced heater power) |
| Absolute max voltage | 33V (TVS clamp) |
| Reverse polarity protection | Yes (high-side PMOS) |
| Surge protection | SMAJ33A TVS, 53V clamp |
| Overcurrent protection | Single user-supplied mini blade input fuse; 2920 PTC on buck branch |
| Idle power (15s RS-485 poll) | 984 µW (41µA @ 24V) |
| Idle power (RS-485 always-on) | 3.6 mW (150µA @ 24V) |

## Heater

| Parameter | Value |
|---|---|
| Channels | 1 (firmware cycles through sensors sequentially) |
| Drive voltage | Input voltage, 10V gate clamp |
| Peak current | 0.6A @ 24V (40Ω heater) |
| Peak power per pulse | 14.4W @ 24V |
| MOSFET | TM15N10D (100V/15A DPAK, 75mΩ) |
| Protection | 4-layer safety: firmware timer, IWDG, TPL5010 external watchdog (~10s), discrete SCR latch (permanent lockout until power cycle) |
| Fusing | Protected by the main mini blade input fuse; no separate heater-branch fuse |
| Flyback protection | SS56 Schottky (60V/5A) |

### Heater power per pulse (40Ω heater, sequential firing)

| Voltage | Current | Power/pulse | Energy (1s pulse) | Energy (8× 1s cycle) |
|---|---|---|---|---|
| 12V | 0.30A | 3.6W | 3.6 J | 28.8 J |
| 24V | 0.60A | 14.4W | 14.4 J | 115.2 J |
| 30V | 0.75A | 22.5W | 22.5 J | 180.0 J |

Peak current and power are the same regardless of sensor count (one heater fires at a time).

## Analog Front End

| Parameter | Value |
|---|---|
| Thermistor channels | 48 |
| ADC | ADS1115, 16-bit sigma-delta |
| Multiplexing | 3× 74HC4067 (16:1 analog mux) |
| Input filtering | 1kΩ + 100nF per channel (fc = 1.6 kHz) |
| ESD protection | 13× PRTR5V0U4D TVS arrays, ferrite-isolated supply |
| Measurement method | Ratiometric (excitation sense on AIN1) |
| Excitation | Switched GPIO, 270Ω + 1µF filtered (fc = 589 Hz) |

### Scan rates

48 channels are topology-agnostic — connect any combination of sensors that fits (e.g., 8× 6-channel sensors, 3× 16-channel sensors, 2× 24-channel sensors, etc.).

| Configuration | Channels | Rate (860 SPS) | Rate (128 SPS) |
|---|---|---|---|
| 1 sensor (6 thermistors + excitation) | 7 | 37 Hz | 8 Hz |
| Full 48 thermistors + excitation | 49 | 5 Hz | 1.2 Hz |

Rates assume mux settling with first-sample discard per channel switch for measurement integrity.

## Maximum Cable Length

Cable length limited by resistive power loss in heater current path. Values for ≤20% heater power loss:

| Wire gauge | Max length (one-way) |
|---|---|
| 20 AWG | 150m |
| 22 AWG | 94m |
| 24 AWG | 59m |

Thermistor readings are unaffected by cable length (ratiometric measurement cancels cable resistance).

## Communications

| Parameter | Value |
|---|---|
| Interface | RS-485 half-duplex (SP3485) |
| Data rate | Up to 10 Mbps |
| Protocol | Binary packed structs over UART (Embassy) |
| Termination | AC (120Ω + 100nF) |
| Bus protection | SM712 asymmetric TVS |
| Power modes | Always-on receiver or firmware-configurable polling |
| Connector | 3-pin screw terminal (A, B, GND) |

## Storage

| Device | Capacity | Purpose |
|---|---|---|
| W25Q256JVFIQ (SPI flash) | 32 MB | Data logging |
| CAT24C32 (I2C EEPROM) | 4 KB | Configuration, calibration |

## MCU

| Parameter | Value |
|---|---|
| Processor | STM32G0B1CBT6 (ARM Cortex-M0+, 64 MHz) |
| Flash | 128 KB |
| RAM | 144 KB |
| RTC | 32.768 kHz crystal, CR2032 backup (0.8µA VBAT, 24+ year coin cell life) |
| Framework | Embassy (async no_std Rust) |

## Expansion

| Interface | Details |
|---|---|
| Aux header (J4) | GND, GND, I2C SDA (1kΩ protected), I2C SCL (1kΩ protected), GPIO PA6, GPIO PA7 |
| SWD header (J6) | VCC, SWDIO, SWCLK, GND |
| Unused MCU pins | PB2, PB10 (I2C2_SCL), PB11 (I2C2_SDA), PB8, PB9, PC13, PA15 |
| Test pads | V3V3, GND, V24_PROT, GATE, PGOOD, NRST, PNP_COLL, EXC |

## Physical

| Parameter | Value |
|---|---|
| Board size | 100 × 65 mm |
| Layers | 6 (Signal / GND / 3.3V / Signal / Signal / Signal) |
| Finish | ENIG |
| Mounting | 4× M3 holes, 92 × 57 mm bolt pattern |

### Board statistics

| Metric | Value |
|---|---|
| Footprints | 246 |
| Pads | 764 |
| Track segments | 1,736 |
| Total trace length | 3.52 m |
| Vias | 571 |
| Copper zones | 13 |

## Cost

### Per-board cost breakdown

|  | 1 unit | 10k units |
|---|---|---|
| BOM (components) | $17.31 | $11.00 |
| Extended part setup fees | $78.00 | amortized |
| PCB fabrication (6-layer ENIG) | $10.00 | $1.50 |
| Assembly setup | $8.00 | amortized |
| Assembly (SMT + TH) | $12.46 | $12.46 |
| **Total per board** | **$125.77** | **$24.96** |

One-time setup fees: $86 (26 extended parts × $3 + $8 assembly). Per-board cost excluding setup: $39.77.

- 53 unique BOM line items, 231 total components
- All parts in stock at JLCPCB

## Comparison vs. Commercial Systems

Baseline: Campbell Scientific CR300 datalogger + Implexx sap flux sensors (representative commercial setup). Heater needle resistance: 40Ω (both systems). Commercial sensors draw 4 mA @ 12V at idle and cannot be powered down.

### System cost

| Sensors | Commercial | SFI v1.1 (1 unit) | SFI v1.1 (10k volume) |
|---|---|---|---|
| 1 | $2,500 | $131 | $30 |
| 4 | $5,500 | $146 | $45 |
| 8 | $9,500 | $166 | $65 |

Commercial: $1,500 logger + $1,000/sensor. SFI: board + $5/sensor (passive thermistors + resistors + cable + housing).

### Battery life (96 Wh battery)

**Commercial system** (12V, 3s heater pulse, sensors always drawing power):

| Sensors | 30 min | 60 min | 120 min |
|---|---|---|---|
| 1 | 20 days | 21 days | 21 days |
| 4 | 11 days | 11 days | 12 days |
| 8 | 7 days | 7 days | 7 days |

Battery life barely changes with measurement interval because sensor idle power (48 mW each) dominates.

**SFI v1.1** (24V, 1s heater pulse, zero sensor idle draw):

| Sensors | 30 min | 60 min | 120 min |
|---|---|---|---|
| 1 | 1.2 yr | 2.2 yr | 3.6 yr |
| 4 | 118 days | 230 days | 1.2 yr |
| 8 | 60 days | 118 days | 230 days |

Battery life scales linearly with measurement interval because heater pulses are the dominant power draw — idle consumption is negligible.

### Side-by-side (30-min interval, 96 Wh)

| Sensors | Commercial | SFI v1.1 | Ratio |
|---|---|---|---|
| 1 | 20 days | 1.2 years | 22× |
| 4 | 11 days | 118 days | 11× |
| 8 | 7 days | 60 days | 9× |

### Power breakdown (8 sensors, 30-min interval)

| Component | Commercial | SFI v1.1 |
|---|---|---|
| Board idle | 144 mW | 0.98 mW |
| Sensor idle (8×) | 384 mW | 0 mW |
| Heater (avg) | 48 mW | 64 mW |
| Measurement (avg) | (incl. in idle) | 1.7 mW |
| **Total** | **576 mW** | **67 mW** |

SFI uses more heater energy per pulse (14.4W × 1s vs. 3.6W × 3s) but eliminates all idle sensor power draw.

### Open hardware & firmware advantages

**Adaptive power management.** Firmware can monitor battery voltage via the onboard 24V divider and dynamically respond:
- Reduce measurement frequency as battery drops
- Pause measurements entirely and coast on <1 mW idle draw indefinitely
- Resume automatically when solar charging restores voltage

**Custom measurement strategies.** Embassy async firmware means the measurement loop, heater timing, and data processing are all user-defined Rust code:
- Conditional measurements (e.g., only measure during daylight, or ramp up frequency during rain events)
- Custom heater pulse profiles (variable duration, multiple pulses, thermal equilibrium detection)
- On-board data reduction before transmission

**Standard interfaces.** RS-485 is the dominant fieldbus in environmental monitoring — compatible with Campbell, Meter, Decagon, and most industrial loggers. The I2C/GPIO breakout header enables:
- External sensors (soil moisture, PAR, humidity)
- LoRa/satellite radio modules for remote telemetry
- Custom expansion boards

**Flexible sensor topology.** 48 thermistor channels, any configuration:
- 8× 6-channel sensors (standard sap flux)
- 3× 16-channel sensors (high-density radial profiles)
- 2× 24-channel sensors
- Mix and match as needed

**Fully open source.** Hardware (KiCad/atopile), firmware (Rust/Embassy), and documentation are all open. No vendor lock-in, no license fees, no EOL risk.

## License

Open source (license TBD).
