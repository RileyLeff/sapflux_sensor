# Sap Flux Sensor Adapter Board — Technical Specifications

## Overview

Open-source adapter board connecting sap flux sensors to a field data logger. Designed for long-duration, unattended forest deployments on 24V solar/battery systems. Firmware built on the Embassy async Rust framework (`no_std`).

## Power

| Parameter | Value |
|---|---|
| Input voltage (continuous) | 5–30V DC |
| Nominal voltage | 24V (12V compatible at reduced heater power) |
| Absolute max voltage | 33V (TVS clamp) |
| Reverse polarity protection | Yes (Schottky) |
| Surge protection | SMAJ33A TVS, 53V clamp |
| Overcurrent protection | 2920 PTC fuse (resettable) |
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
| Fusing | User-supplied 5×20mm glass fuse (clips on board) |
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
| Test pads | V3V3, GND, V24_RAW, GATE, PGOOD, NRST, PNP_COLL, EXC |

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

## License

Open source (license TBD).
