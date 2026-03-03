# Proposed v1.1 Revisions

Changes for the next board revision. v1.0 design documented in
`riley_design.md`. All part numbers verified against JLCPCB stock
as of 2026-02-26.

**v1.0 baseline:** 39 BOM lines, 83 components, ~$116 for 5 boards.

**v1.1 vision:** Scale from a single East30 sensor to **8 independent
sap flux probes** (32 thermistor channels) off one logic board. Target
cost: ~$100/plot for 8-tree coverage. 71x cheaper than commercial
alternatives (~$1,375/tree).

---

## 1. RTC Crystal + Software Temperature Compensation

**Goal:** Autonomous timekeeping with <5 min/year drift when not
connected to a logger.

**Parts to add:**
- Q13FC13500004 (C32346) -- 32.768 kHz crystal, +/-20 ppm, SMD3215-2P,
  basic (no setup fee), $0.18, 659K stock, CL = 12.5 pF
- 2x FH 0402CG180J500NT (C1549) -- 18 pF C0G/NP0 50V, 0402,
  **basic** (no setup fee), $0.001 each, 1.5M stock.
  18 pF gives CL = 12.0 pF assuming 3 pF stray. STM32 RTC CALR
  calibration register compensates +/-488 ppm, so exact cap value is
  non-critical. Tune after first board bring-up if desired.

**Firmware:**
- One-time frequency calibration using STM32 RTC smooth calibration
  register (+/-488 ppm range, 0.954 ppm steps). Eliminates initial
  +/-20 ppm crystal tolerance.
- Software temperature compensation using STM32 internal die temp
  sensor (+/-1.5C). Crystal parabolic model:
  delta_f/f = -0.034 ppm/C^2 * (T - 25)^2
- RS-485 time sync from logger when connected (eliminates long-term drift).

**Expected accuracy:** ~1.3 min/year worst case (0C winter), ~0.3 min/year
at typical forest temps (10-30C). No dedicated crystal temp sensor needed --
MCU sleeps 99.3% of the time so die temp ~= ambient.

**MCU pins:** PC14 (OSC32_IN), PC15 (OSC32_OUT) -- dedicated RTC crystal
pins, no conflict with existing assignments.

**Layout constraints:** Crystal traces < 5 mm. Ground guard ring around
crystal and traces. No switching signals (SPI, mux select, heater gate)
nearby. Load caps placed symmetrically, close to crystal pads.

---

## 2. ADC Architecture: Dual Mux, 32 Thermistor Channels

**Goal:** Support up to 8 sap flux probes (4 thermistors each = 32
channels) with a single ADS1115, using two cascaded 16:1 analog muxes.

**Change:** Remove one ADS1115. Add 2x 74HC4067 16:1 analog muxes.
All thermistor lines route through muxes into ADS1115 AIN0. Excitation
sense goes directly to AIN1 (single-ended ratiometric measurement).

| | v1.0 (2x ADS1115) | v1.1 (1x ADS1115 + 2x mux) |
|---|---|---|
| ADC chips | 2 | 1 |
| Max thermistors | 6 | 32 (2x 16-channel mux) |
| Resolution | 16-bit | 16-bit (same ADC) |
| Interface | I2C | I2C + 4 GPIO (shared select) + 2 GPIO (enables) |
| Idle power | 1 uA | 0.5 uA + ~0 (mux is CMOS, nA leakage) |
| Price delta | -- | -$0.16 (drop ADC + passives, add 2x mux) |

### Mux part

74HC4067PW,118 (C179326) -- Nexperia, 16:1, 80 ohm Ron, TSSOP-24,
31K stock, $0.57 each, extended. Qty 2.

### How it works

Two muxes share select lines S0-S3 (4 GPIOs). Each mux has its own
enable pin (~E, active low). Only one mux is enabled at a time:

```
Thermistors 1-16  → Mux A (channels 0-15) → ──┐
                                                ├→ ADS1115 AIN0
Thermistors 17-32 → Mux B (channels 0-15) → ──┘
```

Firmware cycle: set S0-S3 → enable mux A (disable B) → start ADC →
read result → next channel. After mux A channel 15, switch to mux B.

Enable toggling prevents crosstalk: the disabled mux's output is
high-impedance, so charge injection from its channels can't corrupt
the active measurement.

### Single excitation line

One GPIO pin (PC6) drives all 32 thermistor half-bridges through a
single RC filter:

- R_EXC = 270 ohm (C22966, UNI-ROYAL 0603WAF2700T5E, basic, $0.001)
- C_EXC = 1 uF (C15849, existing BOM)
- Filter cutoff: fc = 1 / (2 * pi * 270 * 1e-6) = 589 Hz

At 8 sensors (32 half-bridges, ~20k each): R_load = 625 ohm.
V_exc = 3.3 * 625 / (625 + 270) = 2.30V. I_steady = 3.7 mA.
I_inrush = 3.3 / 270 = 12.2 mA (safe, under STM32 20 mA GPIO limit).

Firmware: enable PC6 before scan, read all 32 mux channels across
both muxes, disable PC6. Simpler than dual-excitation (no per-group
enable/disable sequencing).

### Excitation sense: ratiometric on AIN1

The single excitation line connects to AIN1 through a 1k series
resistor + 100 nF filter cap (matching the thermistor path protection).
Firmware reads excitation as a **separate single-ended conversion**
(AINp=AIN1, AINn=GND) once per scan cycle, then divides each
thermistor reading by excitation to get ratiometric values:

  T = steinhart_hart(V_therm / V_exc)

Ratiometric measurement cancels excitation voltage uncertainty
(GPIO output variation, R_EXC voltage divider, supply noise). The
2.30V excitation is not a problem — only the ratio matters.

This loses hardware differential CMRR vs v1.0, but:

- Ground bounce is already managed (no ADC during heater pulses)
- Common-mode noise during quiescent reads is minimal (no switching)
- Avoids adding an analog switch IC (saves BOM complexity)
- Temperature differencing (heated - reference) cancels remaining
  systematic errors

AIN2, AIN3: tied to AGND through 1k resistors (prevent noise coupling).

### Scan rates and noise

ADS1115 data rates and noise (PGA = +/-4.096V, LSB = 125 uV):

| Data Rate | Conversion Time | Noise (uVrms) | Temp Noise (mC) | 32-ch Scan | Scan Rate |
|-----------|----------------|---------------|-----------------|------------|-----------|
| 8 SPS | 125 ms | 7.8 | 0.6 | 8.0 s | 0.13 Hz |
| 16 SPS | 62.5 ms | 11 | 0.9 | 4.0 s | 0.25 Hz |
| 32 SPS | 31.3 ms | 16 | 1.3 | 2.0 s | 0.50 Hz |
| 64 SPS | 15.6 ms | 23 | 1.8 | 1.0 s | 1.0 Hz |
| 128 SPS | 7.81 ms | 31 | 2.5 | 0.50 s | 2.0 Hz |
| 250 SPS | 4.0 ms | 47 | 3.8 | 0.26 s | 3.9 Hz |
| 475 SPS | 2.1 ms | 64 | 5.1 | 0.13 s | 7.4 Hz |
| 860 SPS | 1.16 ms | 94 | 7.5 | 0.07 s | 13.4 Hz |

**Note:** Each channel requires a dummy conversion after mux switch
(ADS1115 sampling cap needs to settle). Use fast-dummy trick: run
dummy at 860 SPS (1.16 ms), real conversion at desired SPS. This adds
only 1.16 ms overhead per channel instead of doubling scan time.

**Derivative noise insight:** For sap flux, we measure dT/dt. Derivative
noise = sqrt(2) * temp_noise / dt. Faster scanning is NOT always better
for derivatives -- the noise/dt term grows. Sweet spot: 64-250 SPS.

### Adaptive scan strategy

| Phase | Time Window | Data Rate | 32-ch Rate | Purpose |
|-------|------------|-----------|------------|---------|
| Fast (heating) | 0-5 s | 860 SPS | 13.4 Hz | Capture initial transient |
| Medium | 5-30 s | 250 SPS | 3.9 Hz | Track decay curve |
| Slow (tail) | 30-120 s | 64 SPS | 1.0 Hz | High-precision baseline |

Alternative: always collect at 860 SPS (max speed), filter in software.
Trades flash storage (~16 KB/cycle at 860 SPS) for simplicity. At 32 MB
flash capacity: ~2M measurement cycles = years of data.

### Supported sensor configurations

| Config | Thermistors | Mux A | Mux B | 860 SPS Rate |
|--------|-------------|-------|-------|-------------|
| 1x East30 (4-pin) | 4 | 4 ch | idle | 96 Hz |
| 2x East30 | 8 | 8 ch | idle | 51 Hz |
| 4x East30 | 16 | 16 ch | idle | 26 Hz |
| 8x East30 | 32 | 16 ch | 16 ch | 13 Hz |
| 6x 4-pin generic | 24 | 16 ch | 8 ch | 18 Hz |

Rates assume 2 conversions per mux channel (dummy + real for settling)
plus 1 excitation read per scan cycle.

### MCU pins for mux

| Pin | Function | Notes |
|-----|----------|-------|
| PD0 | Mux select S0 | Shared between both muxes |
| PD1 | Mux select S1 | Shared |
| PD2 | Mux select S2 | Shared |
| PD3 | Mux select S3 | Shared |
| PA9 | Mux A enable (~E) | Active low. 10k pull-up to 3.3V (boot-safe: mux disabled). |
| PA10 | Mux B enable (~E) | Active low. 10k pull-up to 3.3V (boot-safe: mux disabled). |

PB1 freed (was ADC2 ALERT/DRDY in v1.0).

---

## 3. 32 MB NOR Flash

**Goal:** Autonomous data logging for 6-12 months when not connected to
a logger.

**Part:** W25Q256JVFIQ (C779876)
- 32 MB (256 Mbit), SPI, SOIC-16
- 1 uA typical standby / 10 uA max
- Deep power-down: 0.5 uA typical (enter via SPI command between cycles)
- 100K erase cycles, 20-year retention
- $3.92 (qty 10+; $4.26 qty 1), 28K stock, extended

**Pin notes (SOIC-16):**
- Pin 1 RESET#/IO3: tie to VCC (quad-SPI not used; also serves as /HOLD)
- Pin 2 VCC: 3.3V + 100 nF decoupling cap (C14663, existing part)
- Pin 3 RESET#: tie to VCC (active-low reset, keep deasserted)
- Pins 4-6, 11-14: N/C
- Pin 7 CS#: PB12 + 10k pull-up to VCC (boot-safe: flash deselected
  while MCU GPIOs are high-Z during reset)
- Pin 8 DO/IO1: PB14 (MISO)
- Pin 9 WP#/IO2: tie to VCC (no hardware write-protect needed)
- Pin 10 GND
- Pin 15 DI/IO0: PB15 (MOSI)
- Pin 16 CLK: PB13 (SCK)

**Data budget (int16 storage, adaptive scan for 120 s after each 2 s
heater pulse, 30-min intervals, 32 channels):**

| Item | Size |
|------|------|
| Per sample | 32 ch x 2 bytes = 64 bytes |
| Per measurement cycle (adaptive) | ~250 scans x 64 B + 8 B header = ~16 KB |
| Per measurement cycle (860 SPS constant) | ~1,587 scans x 64 B = ~99 KB |
| Per day (48 cycles, adaptive) | ~750 KB |
| 32 MB capacity (adaptive) | ~44 days at 32 ch |
| With 4 channels (1 probe, adaptive) | ~350 days |

Adaptive scan: 860 SPS x 5s (66 scans) + 250 SPS x 25s (96 scans) +
64 SPS x 90s (89 scans) = ~250 scans per cycle. At constant 860 SPS:
~1,587 scans/cycle (~99 KB) = ~7 days at 32 ch.

**MCU pins:** SPI2 bus (PB13=SCK, PB14=MISO, PB15=MOSI) + PB12 (CS).
SPI1 is blocked -- PA5 (SPI1_SCK) is already used by TPL5010 DONE.

---

## 4. Connector Topology, Multi-Heater Support & Gate Driver

**Goal:** Support 1-8 heater probes with safe, efficient high-current
switching. Anti-footgun design: safe at any pulse duration, any number
of heaters.

### Connector layout: two separate blocks on the same board edge

**Heater block (2-pin):** KF2EDGV-3.5-2P (C441321)
- Pin 1: Heater+ (24V switched)
- Pin 2: Heater- (drain return)
- $0.06, 11K stock, extended
- Width: 7 mm

Users wire 1-8 heater elements in parallel at the cable level. The
board just provides switched 24V. Each heater is ~42 ohm, so 8 in
parallel = 5.25 ohm = 4.57 A total.

**Thermistor/signal block:** 2x24 pin header (48P), XFCN PZ254V-12-48P
(C22465947), $0.30, extended.
- 32 thermistor inputs
- GND (thermistor ground return)
- Excitation (GPIO-switched 3.3V via 270 ohm filter)
- Spare pins for: unused GPIOs, SPI bus, I2C bus, etc.
- See Section 11 for full pinout.

Total connector edge footprint: ~70-80 mm. Fits on the board edge.

**Why separate blocks:**
- Physical isolation of high-current (4.57 A) heater path from
  microvolt-level thermistor signals
- Can disconnect heater cable without disturbing thermistor wiring
- Simpler cable assemblies (power wires are thicker gauge)

### Gate driver circuit (NEW in v1.1)

The v1.0 design drives the heater MOSFET gate directly from a 3.3V
MCU GPIO. At v1.1 currents (up to 4.57A for 8 heaters), even a
low-Vgs(th) MOSFET at 3.3V gate has significant Rds(on) and wastes
heat. A gate driver boosts gate voltage to 10V from the existing
24V rail, dropping Rds(on) to spec-sheet values.

**Critical safety requirement (buck-dead fail-safe):** If the 3.3V
buck fails (IC failure, inductor open, output short) while 24V is
still present, the heater MUST remain OFF. Earlier revisions biased
Q_DRIVER from the 3.3V rail (R_PA8_PU), which made all four safety
layers (MCU timer, IWDG, TPL5010, SCR latch) dependent on 3.3V.
If the buck dies, all four fail simultaneously and R_GATE_PULL
charges Q1 gate to 10V with nothing to stop it — heater ON
indefinitely. The current design eliminates this by biasing Q_DRIVER
from the 24V rail via a voltage divider, with PA8 isolated through
Q_CTRL to avoid STM32 ESD clamp back-feed.

**Circuit:**

```
                    R_GATE_PULL (1M)                            Node A
V24_PROT ──────────────────────────────┬───────── Q1 Gate
                                       │
                            D_ZENER (10V, cathode at A, anode to GND)
                                       │
                            Q_DRIVER (drain at A, source to GND)

Q_DRIVER gate: 24V-biased always-on
    V24_PROT ── R_BIAS (4.7M) ──┬── Q_DRIVER gate ── R_GATE_PD (1M) ── GND
                                 │
                            Q_CTRL drain
                            Q_CTRL source ── GND

Q_CTRL gate: MCU-controlled
    PA8 (MCU) ── R_CTRL (10k) ── Q_CTRL gate ── R_CTRL_PD (100k) ── GND
```

**How it works (normal operation):**
1. PA8 LOW → Q_CTRL OFF → Q_DRIVER gate at 4.2V (24V divider) →
   Q_DRIVER ON → drain at ~0V → Q1 gate at ~0V → **heater OFF**
2. PA8 HIGH → Q_CTRL ON → Q_DRIVER gate pulled to ~0V → Q_DRIVER
   OFF → R_GATE_PULL pulls Q1 gate toward 24V → D_ZENER clamps at
   10V → **heater ON** at full Rds(on) spec

**Non-inverted logic (PA8 HIGH = heater ON).** Same polarity as
v1.0. Firmware drives PA8 LOW (default) to keep heater off, then
briefly drives PA8 HIGH for the 2-second heater pulse.

**Why Q_CTRL isolation is required:** An earlier revision connected
PA8 directly to Q_DRIVER gate through a resistor (R_GATE_DRV). The
24V divider on Q_DRIVER gate works correctly in steady state, but
during boot or buck-dead scenarios (V3V3 = 0V), the STM32's internal
ESD protection diode on PA8 forward-biases to VDD, sinking current
from the 24V divider node into the unpowered 3.3V rail. With
R_GATE_DRV = 100k, this pulls Q_DRIVER gate below Vgs(th) (0.99V
vs 1.5V threshold) — defeating the fail-safe in exactly the scenario
it's meant to protect. Increasing R_GATE_DRV doesn't help: values
large enough to limit ESD current (>470k) also prevent PA8 from
overriding the divider to control Q_DRIVER. Q_CTRL breaks this
impasse by isolating PA8 from the divider entirely.

**Boot behavior (24V divider provides fail-safe Q_DRIVER bias):**

R_BIAS (4.7M) and R_GATE_PD (1M) form a voltage divider from
V24_PROT to GND. This is purely resistive — responds instantly to
V24 rising, independent of the 3.3V rail.

1. Power applied. V24_PROT rises. Divider immediately biases
   Q_DRIVER gate at V24 × 1M/5.7M. When V24 reaches ~8.6V (typ
   Vgs(th) = 1.5V × 5.7), Q_DRIVER turns ON → Q1 gate clamped
   to ~0V.
2. R_CTRL_PD (100k) holds Q_CTRL gate at 0V → Q_CTRL OFF → does
   not interfere with Q_DRIVER bias.
3. Buck starts (V24 > ~3.5V UVLO). V3V3 ramps to 3.3V.
4. MCU boots. PA8 defaults to input (high-Z) → Q_CTRL stays OFF →
   Q_DRIVER stays ON → heater stays OFF. No glitch.
5. Only after firmware explicitly drives PA8 HIGH can heater turn ON.

**Boot glitch: none (battery hot-plug).** For step-function V24
(battery connect), the resistive divider biases Q_DRIVER ON
before R_GATE_PULL (1M, τ = 1.8ms) can charge Q1 gate past
Vgs(th). No heater conduction during boot. This is an improvement
over the previous ~1ms glitch.

**Slow-ramp note (solar sunrise):** During a gradual V24 ramp (0V
to 24V over minutes), there is a brief window between V24 ≈ 2.5V
(Q1 Vgs(th)) and V24 ≈ 8.6-14.2V (Q_DRIVER turn-on, varies with
Vgs(th) tolerance) where R_GATE_PULL could charge Q1 gate. At
V24 < 8.6V, heater power is < 4.5W per probe (vs 14.4W nominal).
Starting from cold wood at sunrise, this is not a fire risk.
In practice, the buck starts at V24 ≈ 3.5V and the MCU boots within
~5ms, at which point firmware holds PA8 LOW (heater off).

**MCU crash behavior (SCR latch kill path):** If firmware locks up
with PA8 HIGH (heater stuck on), the TPL5010 watchdog fires within
9.6 seconds. The kill sequence:

1. TPL5010 asserts RST (active low) on MCU NRST pin
2. D_ISO (1N4148WS) blocks RST from reaching the SCR latch directly
   (latch only triggers through the watchdog path, not MCU resets)
3. Q_LATCH (MMBT3904 NPN + SS8550 PNP cross-coupled) latches ON
4. Q_WD (2N7002, driven by latch output) pulls Q1 gate to GND
   **permanently** — heater OFF until power cycle
5. Latch overrides R_GATE_PULL (1M): Q_WD sinks only 24 µA,
   pulling gate to ~0.1 mV
6. MCU still boots and can report the fault via RS-485 (D_ISO
   isolates latch from NRST so MCU reset works normally)

**Buck-dead behavior (the scenario that motivated this redesign):**
If 24V is present but 3.3V is dead, the 24V divider holds Q_DRIVER
gate at 4.2V → Q_DRIVER ON → Q1 gate at ~0V → heater OFF. No MCU,
watchdog, or latch needed. The heater is OFF by physics alone.

**Recovery:** Power cycle only. The SCR latch is self-sustaining
once triggered — R_FB (100k) provides base current to maintain the
cross-coupled state. R_LATCH_PD (1M) bleeds the latch when VDD
is removed. This is intentional: a latched heater fault should
require human intervention, not auto-recover.

**State table:**

| State | Q_CTRL | Q_DRIVER | Heater | Safe? |
|-------|--------|----------|--------|-------|
| Sleep (PA8 LOW) | OFF | ON | OFF | Yes |
| Heater pulse (PA8 HIGH) | ON | OFF | ON | Controlled |
| Boot (PA8 float) | OFF | ON | OFF | Yes |
| Buck dead, 24V present | OFF | ON | OFF | Yes |
| MCU crash, PA8 HIGH | ON | OFF | ON | SCR latch 9.6s |
| MCU crash, PA8 LOW | OFF | ON | OFF | Yes |

**Interaction analysis:** The Q_CTRL topology does not affect any
other circuit path. SCR latch (Q_WD on Q1 gate) works unchanged —
Q_WD sinks 24µA from R_GATE_PULL at Rds ~5Ω. PGOOD-gated latch
arming is on a separate node. Sleep current: +4.2µA from 24V
(divider) vs current design. Firmware polarity matches v1.0 (PA8
HIGH = heater ON), simplifying the firmware port.

**Parts:**
- Q_DRIVER: 2N7002 (C8545, already in BOM) -- N-channel, SOT-23
- Q_CTRL: 2N7002 (C8545, already in BOM) -- MCU isolation
- D_ZENER: MM3Z10 (C19077459, preferred, $0.012) -- 10V, SOD-323
- R_GATE_PULL: 1M (C22935, already in BOM)
- R_BIAS: 4.7M (C23163, preferred, $0.0014) -- 24V divider
- R_GATE_PD: 1M (C22935, already in BOM)
- R_CTRL: 10k (C25804, already in BOM) -- PA8 to Q_CTRL gate
- R_CTRL_PD: 100k (C25803, already in BOM) -- boot-safe pull-down

Net new unique parts: 1 (zener diode). All resistors are existing
values. v1.0's R_GATE (1k) and R4 (10k gate pulldown) are removed —
Q_DRIVER replaces both functions.

### MOSFET upgrade

**Current (v1.0):** XCHS3N12 (C48928121), 120V/3A, SOT-23, $0.03
**New:** TM15N10D (C7422818), 100V/15A, DPAK (TO-252-3L), $0.087

| Parameter | v1.0 (3.3V gate) | v1.1 (10V gate via driver) |
|-----------|------------------|---------------------------|
| Vgs(th) | 0.5-1.5V | 1.0-2.0V |
| Rds(on) | ~267 mohm @ 3.3V (est.) | 75 mohm @ 10V (spec) |
| Pdiss @ 1 heater | 87 mW | 24 mW |
| Pdiss @ 2 heaters | 350 mW | 98 mW |
| Pdiss @ 4 heaters | 1.4 W | 391 mW |
| Pdiss @ 8 heaters | 5.6 W (unsafe) | 1,565 mW |
| Package | SOT-23 | DPAK (TO-252-3L) |

**Thermal at 8 heaters:** 1.57W × 20°C/W (DPAK Rth,j-a) = 31°C rise.
At 40°C ambient: Tj = 71°C. At 50°C: Tj = 81°C. Max Tj = 175°C —
comfortable margin. Not trivial dissipation (1.57W), but well within
DPAK capability for 2-second pulses every 30 minutes.

DPAK thermal pad: exposed drain tab must be soldered to copper pour
with thermal vias to inner layers. Standard DPAK practice.

### D1 (reverse polarity Schottky) upgrade

**CRITICAL FIX:** D1 is in the SERIES power path. Topology from
protection.ato: `J1 → PTC → TVS → D1 → V24_PROT → {buck, heater}`.
D1 carries ALL current including heater (up to 4.57A for 8 probes).
The v1.0 SS36 (3A) is undersized for v1.1.

- **Current:** SS36 (C16015), 3A/60V, SMA
- **New:** SS56 (C18199169), 5A/60V, SMA, **preferred** (no setup fee), $0.045
- Same SMA footprint -- drop-in replacement
- 5A continuous + 120A surge handles 4.57A with margin

**Thermal constraint at 8 heaters:** At 4.57A, Vf ~0.65V →
~3.0W dissipation during the 2-second pulse. SMA transient thermal
impedance at 2s is ~30% of steady-state Rth(j-a): junction rise
~80°C. At 40°C ambient (sun-heated enclosure): Tj ~120°C, leaving
30°C margin to Tj,max=150°C. Acceptable for 2-second pulses every
30 minutes (average power: 0.003W). In extreme heat (>50°C ambient),
margin narrows to ~20°C -- monitor D1 temp on first field boards.

**Layout mitigation:** Place 4-6 thermal vias (0.3mm drill) under
D1's SMA anode pad, connected to the GND plane (In1.Cu). Add a
copper pour on B.Cu under the diode for additional heat spreading.
This reduces Rth(j-a) by ~30-40%, improving the hot-ambient margin.

**Fallback:** If thermal testing at 8 heaters in >50°C ambient
shows Tj approaching limits, upgrade to a higher-rated SMA Schottky
(e.g., SS56 SMA with better thermal specs). Note: SMA and SMC
footprints are NOT compatible (SMA: 4.3x2.6mm, SMC: 7.7x5.9mm).
An SMC upgrade would require a board respin.

### PTC fuse split topology

**Problem:** v1.0 PTC (1.1A hold) is in the series path before both the
buck converter and heater. At 2+ heaters (1.14A), the PTC trips.

**v1.1 topology:**
```
                          ┌→ PTC (1.1A) → buck input (20 mA steady)
J1 → TVS → Schottky → V24_PROT ─┤
                          └→ [6A fuse footprint] → heater MOSFET (4.57A pulsed)
```

- PTC moves from the main series input path (v1.0: before TVS/Schottky)
  to the buck-only branch (v1.1: after the V24_PROT split). It now
  protects only the electronics path (20 mA steady), not the heater.
- Heater path gets a **dedicated fuse** (6A fast-blow, 5x20mm).
  Populated by default — not optional. The fuse is the last line of
  defense for catastrophic shorts (e.g., cable insulation failure in
  the field). User-supplied: any 5-6A fast-blow 5x20mm glass fuse.
- The 4-layer watchdog/latch system handles normal fault modes. The
  fuse handles wiring faults that bypass the electronics entirely.
- Combined input (TVS + Schottky) sees full current (heater + buck)
  during pulses. The TVS only conducts during transients (microamp
  leakage in steady state). The SS56 Schottky handles 4.57A -- see
  thermal note below.

### Multi-heater table

| Heaters | Total current | MOSFET Pdiss (75mΩ @ 10V) | D1 load | Status |
|---------|--------------|---------------------------|---------|--------|
| 1 | 0.57 A | 24 mW | 11% | Easy |
| 2 | 1.14 A | 98 mW | 23% | Easy |
| 4 | 2.29 A | 391 mW | 46% | OK (Tj rise ~8°C) |
| 8 | 4.57 A | 1,565 mW | 91% | OK (Tj rise ~31°C) |

---

## 5. Input Protection (All Channels)

**Goal:** Identical ESD/EMI protection on all 33 connector-facing analog
lines (32 thermistors + 1 excitation).

Each line gets three protection stages:

1. **TVS array (PRTR5V0U4D):** ESD clamp at connector side. Each IC
   protects 4 lines. 33 lines / 4 = 9 arrays (3 spare channels).
   - v1.0: 2 arrays. v1.1: 9 arrays. Add 7x PRTR5V0U4D (C2835429, $0.06 each).

2. **1k ohm series resistor:** Current limiter + RC filter partner.
   Limits fault current to 3.3 mA. 33 resistors total.
   - v1.0: 7 resistors. v1.1: 33. Add 26x 1k ohm 0603 (C21190, $0.01 each).

3. **100 nF filter cap:** Low-pass filter (fc = 1.6 kHz with 1k ohm).
   Blocks EMI from cables. 33 caps total.
   - v1.0: 7 caps. v1.1: 33. Add 26x 100 nF 0603 (C14663, $0.01 each).

**Zero measurement impact:** The 1k ohm series resistance appears on
BOTH thermistor sense lines AND the excitation sense line (AIN1). In
the ratiometric calculation (V_therm / V_exc), identical voltage
dividers formed by 1k + ADS1115 input impedance (>1 GOhm) cancel.
Mux Ron (~80 ohm) appears only in the thermistor path, but with >1 GOhm
load, the voltage drop is negligible (16 uV from input leakage).

**Unused mux channels:** Tie to GND. Prevents floating CMOS inputs
from coupling noise.

**Cost for additional channels:** +$0.94 (7 TVS + 26 R + 26 C)

---

## 6. GPIO Pin Assignment (STM32G0B1CBT6, LQFP-48)

### v1.0 assignments (unchanged unless noted)

| Pin | Function | Peripheral | v1.1 Change |
|-----|----------|------------|-------------|
| PA0 | RS-485 ~RE | GPIO | -- |
| PA1 | RS-485 DE | GPIO | -- |
| PA2 | RS-485 TX | USART2_TX | -- |
| PA3 | RS-485 RX | USART2_RX | -- |
| PA4 | TPL5010 WAKE | GPIO | -- |
| PA5 | TPL5010 DONE | GPIO | -- |
| PA8 | Heater gate driver | GPIO | Now drives 2N7002 gate (inverted: LOW=on) |
| PA13 | SWD data | SWDIO | -- |
| PA14 | SWD clock / BOOT0 | SWCLK | -- |
| PB0 | ADC ALERT/DRDY | GPIO (interrupt) | -- |
| PB3 | LED red (sink) | GPIO | -- |
| PB4 | LED green (sink) | GPIO | -- |
| PB5 | LED blue (sink) | GPIO | -- |
| PB6 | I2C clock | I2C1_SCL | -- |
| PB7 | I2C data | I2C1_SDA | -- |
| PC6 | Excitation enable | GPIO | -- |
| PC7 | PGOOD input | GPIO | -- |

### v1.1 new assignments

| Pin | Function | Peripheral | Notes |
|-----|----------|------------|-------|
| PB12 | Flash CS | SPI2_NSS | Active low |
| PB13 | Flash SCK | SPI2_SCK | |
| PB14 | Flash MISO | SPI2_MISO | |
| PB15 | Flash MOSI | SPI2_MOSI | |
| PD0 | Mux select S0 | GPIO | Shared S0-S3 for both muxes |
| PD1 | Mux select S1 | GPIO | |
| PD2 | Mux select S2 | GPIO | |
| PD3 | Mux select S3 | GPIO | |
| PA9 | Mux A enable (~E) | GPIO | Active low, 10k pull-up (boot-safe) |
| PA10 | Mux B enable (~E) | GPIO | Active low, 10k pull-up (boot-safe) |
| PA12 | EEPROM WP | GPIO | Active high = write protect. 10k pull-up (boot-safe: writes blocked until firmware enables) |
| PC14 | RTC crystal | OSC32_IN | Dedicated |
| PC15 | RTC crystal | OSC32_OUT | Dedicated |

### Power pin changes

- VBAT (pin 1): disconnected from VDDA_FILT, routed through BAT54S
  from VDD and CR2032

### Freed pins

- PB1: was ADC2 ALERT/DRDY (2nd ADS1115 removed)

### Spare GPIOs (13 total)

PA6, PA7, PA11, PA15, PB1, PB2, PB8, PB9, PB10, PB11, PC13,
PF0, PF1

**LQFP-48 port availability (verified):** PA0-PA15, PB0-PB15,
PC6, PC7, PC13, PC14, PC15, PD0-PD3, PF0, PF1. No other PC or PD
pins exist on this package (PC0-PC5, PC8-PC12, PD4+ are only on
LQFP-64/100).

**SPI1 conflict note:** SPI1_SCK maps to PA5, which is already used by
TPL5010 DONE. SPI2 on PB12-PB15 avoids this conflict entirely.

**I2C note:** Run at 100 kHz (standard mode). With single ADS1115's
10k pull-ups, rise time is 424 ns at 50 pF bus -- fine for standard
mode (spec: < 1000 ns) but marginal for 400 kHz fast mode (spec:
< 300 ns). No hardware change needed. If external I2C sensors are
connected via the 48-pin header (pins 43-44) on long cables, the
added bus capacitance may require stronger pull-ups (4.7k or 2.2k).
Evaluate after first field deployment.

---

## 7. BOM Impact Summary

### Parts added

| Part | LCSC | Qty | Unit cost | Total | Library |
|------|------|-----|-----------|-------|---------|
| 32.768 kHz crystal | C32346 | 1 | $0.18 | $0.18 | basic |
| 18 pF C0G 0402 load caps | C1549 | 2 | $0.001 | $0.002 | basic |
| 74HC4067 mux | C179326 | 2 | $0.57 | $1.14 | extended |
| W25Q256JVFIQ flash | C779876 | 1 | $3.92 | $3.92 | extended |
| TM15N10D MOSFET | C7422818 | 1 | $0.09 | $0.09 | extended |
| SS56 flyback diode | C18199169 | 1 | $0.05 | $0.05 | preferred |
| SS56 reverse polarity (D1) | C18199169 | 1 | $0.05 | $0.05 | preferred |
| MM3Z10 10V zener (gate driver) | C19077459 | 1 | $0.012 | $0.012 | preferred |
| 5x20mm fuse clips (heater) | C3130 | 2 | $0.027 | $0.054 | extended |
| 2x24 pin header (48P, 2.54mm) | C22465947 | 1 | $0.30 | $0.30 | extended |
| KF2EDGV-3.5-2P heater conn | C441321 | 1 | $0.06 | $0.06 | extended |
| PRTR5V0U4D TVS | C2835429 | 7 | $0.06 | $0.42 | extended |
| 1k ohm 0603 | C21190 | 26 | $0.01 | $0.26 | basic |
| 100 nF 0603 | C14663 | 29 | $0.01 | $0.29 | basic |
| CR2032 holder (SMD) | C7498149 | 1 | $0.21 | $0.21 | extended |
| BAT54S Schottky | C7420333 | 1 | $0.01 | $0.01 | preferred |
| 270 ohm 0603 (R_EXC, replaces 560 ohm) | C22966 | 1 | $0.001 | $0.001 | basic |
| 100 nF decoupling (mux x2, flash x1) | C14663 | (counted above) | -- | -- | basic |
| 1k resistors (AIN2, AIN3 tie to AGND) | C21190 | (counted above) | -- | -- | basic |
| 4.7M R_BIAS (gate driver 24V divider) | C23163 | 1 | $0.001 | $0.001 | preferred |
| 100k R_CTRL_PD (Q_CTRL gate pull-down) | C25803 | (counted in 100k) | -- | -- | basic |
| 10k pull-ups (mux ~E x2, flash CS, EEPROM WP) | C25804 | 4 | $0.01 | $0.04 | basic |
| 1 uF VBAT buffer cap | C15849 | (counted above) | -- | -- | basic |

**Note:** Gate driver uses Q_DRIVER + Q_CTRL (both 2N7002, C8545,
existing BOM). R_GATE_PULL and R_GATE_PD are 1M (C22935, existing).
R_CTRL is 10k (C25804, existing). R_CTRL_PD is 100k (C25803,
existing). Only new part number: R_BIAS 4.7M (C23163, preferred,
no setup fee). The 4x 10k pull-ups (mux ~E x2, flash CS, EEPROM WP)
use existing C25804. The 270 ohm R_EXC (C22966) replaces the v1.0
560 ohm R_EXC (C23204) — value swap, not a new line item. The VBAT
1 uF cap uses existing C15849.

### Parts removed

| Part | LCSC | Qty | Unit cost | Saved |
|------|------|-----|-----------|-------|
| ADS1115 (2nd) | C37593 | 1 | $1.30 | $1.30 |
| ADS1115 passives (2nd) | various | ~4 | ~$0.03 | $0.10 |
| XCHS3N12 MOSFET | C48928121 | 1 | $0.03 | $0.03 |
| SS36 (D6 flyback) | C16015 | 1 | $0.05 | $0.05 |
| SS36 (D1 reverse polarity) | C16015 | 1 | $0.04 | $0.04 |
| R_GATE (1k, MCU→Q1 gate) | C21190 | 1 | $0.01 | $0.01 |
| R4 (10k, Q1 gate pulldown) | C25804 | 1 | $0.01 | $0.01 |
| R_EXC (560 ohm, replaced by 270 ohm) | C23204 | 1 | $0.001 | $0.001 |
| KF2EDGV-3.5-10P | C441318 | 1 | $0.19 | $0.19 |

### Net cost delta

| | Per board | Notes |
|---|----------|-------|
| Parts added | +$7.10 | |
| Parts removed | -$1.71 | |
| **Net parts delta** | **+$5.37** | |
| Extended part fees | +$24.00 | 8 new unique extended parts x $3 |
| | | (flash, mux, MOSFET, fuse clips, 48P header, 2P heater, PRTR5V0U4D, CR2032) |

**Per-board cost at 5 boards:** ~$5.37 + ($24/5) = **$10.17/board**
**Per-board cost at 50 boards:** ~$5.37 + ($24/50) = **$5.85/board**

**Through-hole assembly note:** The M8 connector, 48-pin header, 2-pin
heater terminal, and fuse clips are TH parts. JLCPCB charges extra for
TH assembly (~$0.20/joint). Budget ~$2-3/board for TH assembly surcharge.

**Stock warnings (as of 2026-03):**
- PRTR5V0U4D (C2835429): 2,617 stock. At 7 per board, supports ~374
  boards. Fine for prototyping but verify before production runs.
  Backup: NXP PRTR5V0U4D (original, typically higher stock).
- 48-pin header (C22465947): 817 stock. Fine for 5-50 boards. Backup:
  any 2x24 2.54mm pin header (common form factor, many vendors).

**User-supplied (not in BOM):** CR2032 coin cell (~$0.30 retail),
5x20mm fast-blow fuse (5-6A).
5x20mm glass fuse for heater path (~$0.10, any 5-6A fast-blow).

---

## 8. CR2032 VBAT Backup (RTC + Backup Registers)

**Goal:** Keep the RTC running (with calibration intact) across power
cycles and during extended shelf storage, without requiring users to set
the clock at deployment time.

### Why this matters (the Campbell footgun)

Campbell dataloggers use a CR2032 to keep both the RTC *and* RAM alive.
Their VBAT load is 50-100 uA, which drains the coin cell in 3-5 years.
Users routinely discover dead clocks at deployment. Our design draws
**0.8 uA** from VBAT (RTC + LSE oscillator only, no RAM). The CR2032
lasts 24+ years on self-discharge alone.

### STM32G0B1 VBAT domain

| Resource | Survives on VBAT | Notes |
|----------|-----------------|-------|
| RTC (counter + alarms) | Yes | Autonomous 32.768 kHz counter |
| RTC CALR (smooth calibration) | Yes | Hardware pulse add/remove runs autonomously |
| LSE oscillator | Yes | Powers the 32.768 kHz crystal |
| TAMP registers | Yes | Tamper detection + 5 backup registers |
| 5 backup registers (20 bytes) | Yes | General-purpose 32-bit words |
| Temperature compensation firmware | **No** | Needs MCU core + ADC (VDD domain) |
| RAM | **No** | All RAM powered by VDD |

**Key insight:** CALR *does* survive on VBAT. Factory calibration runs
autonomously via hardware pulse correction. Only dynamic temperature
compensation stops when VDD is removed -- OK because shelf storage is
at roughly constant temperature.

### Shelf drift analysis

| Scenario | Temp | Annual drift | 2 years |
|----------|------|-------------|---------|
| Warehouse (20-25C) | 25C | 0.3 s | 0.6 s |
| Cool warehouse | 15C | 3.7 s | 7.3 s |
| Worst case (0C) | 0C | 21.3 s | 42.5 s |

Even worst case: 42 seconds drift over 2 years. Corrected on first
RS-485 contact.

### UX flow

1. **Factory:** Board assembled, CR2032 installed, RTC set to UTC,
   smooth calibration performed. Temperature compensation running.
2. **Shelf:** CR2032 keeps RTC alive with static CALR correction.
   Drift: 0-40 seconds per year at room temp.
3. **Deployment:** User connects 24V. Temperature compensation resumes.
   Clock accurate to <1 minute.
4. **Logger sync:** First RS-485 connection zeros out shelf drift.
5. **LED indication:** Yellow blink = time not sync'd since power-up.
   Green blink = time sync'd.

### Backup register allocation

| Register | Contents | Notes |
|----------|----------|-------|
| BKP0 | Last calibration temp (Q16.16) | For temperature compensation restart |
| BKP1 | Calibration timestamp | When CALR was last tuned |
| BKP2 | Boot counter + flags | Cold boot detection, dirty shutdown |
| BKP3 | Last logger sync timestamp | For drift tracking |
| BKP4 | Reserved | Future use |

### Circuit

```
VDD (3.3V) ─── Anode 1 ──┐
                          ├── Common cathode ──── VBAT pin (STM32)
CR2032+ ────── Anode 2 ──┘
CR2032- ────── GND

Plus 100nF + 1uF decoupling caps on VBAT pin.
```

**Parts:**
- BAT54S (C7420333) -- dual series Schottky, SOT-23, preferred, $0.012
- BS-CR2032-8 (C7498149) -- SMD coin cell holder, $0.21, extended
- 100 nF decoupling cap -- C14663, already in BOM
- 1 uF buffer cap -- C15849 (1uF 50V X5R 0603, basic, $0.01)

The 1 uF cap provides bulk charge during VBAT domain switchover
(VDD drops, BAT54S commutates to CR2032). Without it, a brief
voltage dip on VBAT during the transition could glitch the RTC
counter. The 100 nF handles high-frequency decoupling; the 1 uF
handles the transient current spike.

### Cold weather performance

| Temp | CR2032 voltage | VBAT (after Schottky) | Margin above 1.55V |
|------|---------------|----------------------|---------------------|
| 25C | 3.0V | 2.7V | 1.15V |
| 0C | 2.95V | 2.65V | 1.10V |
| -20C | 2.85V | 2.55V | 1.00V |
| -30C | 2.75V | 2.45V | 0.90V |

### MCU pin: VBAT (pin 1 on LQFP-48)

Dedicated power pin. v1.1 disconnects VBAT from ferrite path and routes
through BAT54S instead. VBAT is protected from input transients:
VDD comes from the regulated 3.3V buck output (already filtered), and
the BAT54S provides reverse blocking from both paths.

---

## 9. RS-485 Auto-Wake UX

**Goal:** Zero-configuration RS-485 responsiveness.

### The problem

RS-485 receiver (SP3485) draws 800 uA always-on. In polling mode
(receiver off, wake every 10s), it drops to ~15-20 uA. But a 10-second
response lag is terrible UX when connecting a laptop in the field.

### Solution: adaptive mode switching

```
                        ┌─────────────────┐
          power on      │  ALWAYS-ON MODE │
        ──────────────► │  (10s safety    │
                        │   boot window)  │
                        └───────┬─────────┘
                                │ 10s expires, no contact
                                ▼
                        ┌─────────────────┐
                        │  POLLING MODE   │◄── idle timer expires
                        │  (10s wake, RX  │    (default 5 min)
                        │   off between)  │
                        └───────┬─────────┘
                                │ RS-485 frame detected
                                │ during polling window
                                ▼
                        ┌─────────────────┐
                        │  ALWAYS-ON MODE │◄── any TX or RX
                        │  (RX enabled,   │    resets idle timer
                        │   instant resp) │
                        └───────┬─────────┘
                                │ idle timer expires
                                │ (configurable, default 5 min)
                                ▼
                        ┌─────────────────┐
                        │  POLLING MODE   │
                        └─────────────────┘
```

### How it works

1. **Boot:** Enters ALWAYS-ON mode for 10 seconds. This lets a connected
   logger immediately establish contact without waiting for a polling
   window. (This is the safety boot window.)
2. **No contact after 10s:** Drops to POLLING mode. MCU wakes every
   10 seconds (EEPROM-configurable), enables receiver for ~5 ms,
   checks for bus activity, goes back to sleep.
3. **First contact:** During a polling wake window, the MCU sees a valid
   MODBUS frame. Switches to ALWAYS-ON mode. Responds immediately.
   Starts idle timer.
4. **Active session:** Every TX or RX resets the idle timer. Response
   latency: <1 ms (USART interrupt from Stop mode).
5. **Session end:** If idle timer expires (default 5 minutes, EEPROM-
   configurable), drop back to POLLING mode.

### Power impact

| Mode | Current from 24V | Notes |
|------|-----------------|-------|
| Polling (default) | ~15-20 uA | Wake every 10s, ~0.05% duty |
| Always-on | ~130-243 uA | SP3485 receiver + USART clocked |

At 8 heaters, heater energy dominates (94% of cycle budget). The
difference between modes is <1 day of battery life out of ~31 days.

### Why no physical button

The first RS-485 frame *is* the button. 10-second worst-case first
contact delay is acceptable. No mechanical failure points.

---

## 10. Sleep Power Budget Update

v1.1 changes vs v1.0:
- Remove 1x ADS1115: -0.5 uA
- Add flash (deep power-down): +0.5 uA
- Add 2x mux: +~0 uA (nA leakage)
- Add crystal/LSE: on VBAT, not main supply
- Gate driver quiescent: +28.2 uA from 24V (24 uA through
  R_GATE_PULL/Q_DRIVER + 4.2 uA through R_BIAS/R_GATE_PD divider).
  This is ~0.7 uA referred to 3.3V equivalent via buck efficiency.
- Net change: ~+0.7 uA from 3.3V equivalent (wash)

Bottom line doesn't change materially from v1.0. The gate driver's
28µA from the 24V rail is comparable to v1.0's ~20µA sleep budget.

---

## 11. Connector Pinout (48-pin, 2x24, 2.54mm)

**Part:** XFCN PZ254V-12-48P (C22465947) -- 2x24 pin header, 2.54mm
pitch, through-hole, $0.30, extended.

**Board footprint:** 61mm wide x ~5mm deep. Sits on board edge alongside
the 2-pin heater screw terminal (7mm). Total edge: ~73mm, fits on
100mm board.

**Heater connector** is separate: KF2EDGV-3.5-2P (C441321), 2-pin screw
terminal, rated 10A. Carries up to 4.57A (8 heaters in parallel) for
2-second pulses. 46% of 10A rating = comfortable margin.

### Pinout

```
Pin    Row A (odd)  |  Row B (even)    Pin
--- --------------  |  -------------- ---
  1            GND  |  EXC              2     Sensor 1
  3        Therm 1  |  Therm 2          4
  5        Therm 3  |  Therm 4          6
  7        Therm 5  |  Therm 6          8     Sensor 2
  9        Therm 7  |  Therm 8         10
 11            GND  |  GND             12     ground fence
 13        Therm 9  |  Therm 10        14     Sensor 3
 15       Therm 11  |  Therm 12        16
 17       Therm 13  |  Therm 14        18     Sensor 4
 19       Therm 15  |  Therm 16        20
 --- -- mux A/B boundary ------------ ---
 21            GND  |  EXC             22     Sensor 5
 23       Therm 17  |  Therm 18        24
 25       Therm 19  |  Therm 20        26
 27       Therm 21  |  Therm 22        28     Sensor 6
 29       Therm 23  |  Therm 24        30
 31            GND  |  GND             32     ground fence
 33       Therm 25  |  Therm 26        34     Sensor 7
 35       Therm 27  |  Therm 28        36
 37       Therm 29  |  Therm 30        38     Sensor 8
 39       Therm 31  |  Therm 32        40
 --- -- auxiliary ------------------- ---
 41            GND  |  GND             42
 43        I2C SDA  |  I2C SCL         44
 45     GPIO (PA6)  |  GPIO (PA7)      46
 47     GPIO (PB1)  |  GPIO (PB2)      48
```

**Signal summary:** 32 thermistors, 2 excitation (same net, pin 2+22),
8 GND, 2 I2C, 4 GPIO. Both EXC pins carry the same signal — wired to
the same filtered excitation line. Two pins for convenience (one per
mux group's cable bundle).

**Design rationale:**
- Thermistors grouped in blocks of 4 (one sensor per block). Each
  sensor's wires land on consecutive pins -- breakout board traces
  run straight with zero crossings.
- Mux A sensors (1-4) on pins 1-20, mux B sensors (5-8) on pins
  21-40. Mirrored layout.
- GND fences every 8-10 pins. Double-GND at mux boundary (11/12)
  and mid-group (31/32) for crosstalk isolation.
- Single excitation: one PC6 GPIO drives all 32 half-bridges through
  270 ohm filter (3.7 mA steady). Pin 2 and pin 22 are the same net
  — two physical pins for cable routing convenience.
- Auxiliary block (41-48): I2C for external sensors, 4 spare GPIOs,
  extra GND pair.
- No 3.3V export (PFM-mode buck has ~23 mA headroom during scan but
  ripple is unsuitable for external analog loads; unknown external
  loads could also exceed 50 mA limit during flash writes).

### Physical interface

**Lab/bench testing:** Push Dupont jumper wires or crimp housings
directly onto pin header. No breakout board needed. Wire one sensor
in 30 seconds.

**Field deployment:** IDC ribbon cable from pin header to a breakout
board inside the weatherproof enclosure. Breakout board has screw
terminals grouped per sensor position. Sensor cables enter enclosure
through IP68 cable glands.

**Breakout board** (separate, simple PCB -- just connectors and traces):
- 8 groups of 6-position screw terminals (GND + EXC + 4 thermistors)
- GND and excitation bussed internally per mux group
- Connects to logic board via ~10cm IDC ribbon cable
- Different breakout variants for different deployments (2-sensor,
  4-sensor, 8-sensor)
- Cost: ~$3-5 for PCB + connectors

---

## 13. SWD Header & Test Points

### SWD programming header

4-pin through-hole header (2.54mm pitch) for standard breadboard-style
pin strip. Locate near the MCU, away from the board edge connectors.

| Pin | Signal | MCU Pin | Notes |
|-----|--------|---------|-------|
| 1 | SWDIO | PA13 | Data |
| 2 | SWCLK | PA14 | Clock |
| 3 | GND | -- | Probe ground reference |
| 4 | NRST | NRST | Optional but useful for halt/reset |

Use with ST-Link V2 or J-Link. No boot button needed — SWD can
always halt the core, erase, and reprogram regardless of firmware
state (unless debug port is locked via option bytes, which we won't
do during development).

**Optional 5th pin:** Add V3V3 so the probe can detect target power
without a separate connection. Not required for programming.

### Test points

Through-hole pads (1mm drill, 2mm annular ring) for scope/DMM probes.
Group by function on the PCB. One GND test point per group for short
probe ground leads.

**Power rail verification (first thing to check on a new board):**

| Label | Net | Expected | What it tells you |
|-------|-----|----------|-------------------|
| TP_V24 | V24_PROT | 12-29V | Input power through protection |
| TP_3V3 | V3V3 | 3.3V ±2% | Buck converter running |
| TP_PGOOD | PGOOD | HIGH when V3V3 stable | Buck startup complete |
| TP_GND1 | GND | 0V | Power section probe reference |

**Analog section (verify ADC signal chain):**

| Label | Net | Expected | What it tells you |
|-------|-----|----------|-------------------|
| TP_VDDA | VDDA_FILT | 3.3V (clean) | Ferrite bead filtering |
| TP_EXC | excitation | ~2.3V (loaded) | Thermistor excitation active |
| TP_GND2 | GND | 0V | Analog section probe reference |

**Heater & safety (verify gate driver and latch):**

| Label | Net | Expected | What it tells you |
|-------|-----|----------|-------------------|
| TP_GATE | Q1 gate | 0V idle, 10V firing | Gate driver working |
| TP_LATCH | SCR latch output | LOW normal, HIGH latched | Latch state |
| TP_GND3 | GND | 0V | Heater section probe reference |

**Bring-up sequence:**
1. Apply 24V. Check TP_V24 (should match input).
2. Check TP_3V3 (3.3V) and TP_PGOOD (HIGH). If not: buck problem.
3. Connect SWD, flash firmware.
4. Check TP_EXC with excitation enabled (should be ~2.3-3.1V depending
   on number of thermistors connected).
5. Check TP_GATE: should be 0V idle, 10V during heater pulse.
6. Verify TP_LATCH stays LOW during normal operation.

**Board area:** SWD header ~2.5x10mm. 9 test points at ~5mm² each ≈
45mm² total. Negligible impact on 90x62mm board.

---

## Resolved Items (formerly open)

### PTC inrush: non-issue

Peak inrush: 10-37A depending on cable length, duration 200-700 us.
PTC fuses are thermal devices -- need 100+ ms of sustained overcurrent
to trip. Sub-millisecond capacitor charging pulse cannot trip a PTC.
Steady-state through PTC: ~20 mA (55x below 1.1A hold). Keep as-is.

### Board size: 100 x 80 mm

At 40% component density, estimated minimum area is ~8,630 mm^2.
100x80 = 8,000 mm^2 (tight but feasible with CR2032 on back side).
Stays within JLCPCB's cheapest 100x100mm tier. If layout is tight,
100x90mm gives comfortable margin.

### Extended part fee optimization: not worth pursuing

No basic/preferred alternatives exist for 74HC4067, flash, MOSFET,
or mechanical connectors. The $3 extended fees are one-time per order
(not per board). At 50 boards: $0.42/board. Rounding error vs the
$3.92 flash chip.

---

## Resolved Items (second batch)

### Excitation simplified to single line

Originally proposed as dual excitation (EXC_A on PC6, EXC_B on PA11)
with both connected to AIN1 through resistors. External review found
that the dual-line AIN1 topology created a summing node — when the
inactive GPIO was low, AIN1 read ~2.0V instead of the expected
excitation voltage.

**Fix:** Single excitation line (PC6 only, 270 ohm R_EXC). Eliminates
the AIN1 summing bug entirely. Frees PA11 as spare GPIO. R_EXC reduced
from 560 to 270 ohm to maintain adequate V_exc (2.30V) at 32 bridges.
Ratiometric measurement (V_therm / V_exc) cancels the voltage divider.

### Pin availability: PD4, PD5, PC8 do not exist on LQFP-48

Verified against atopile package definition. LQFP-48 has only:
PC6, PC7, PC13, PC14, PC15 (no PC0-PC5 or PC8-PC12) and PD0-PD3
(no PD4+). Reassigned: PD4→PA9 (mux A ~E), PD5→PA10 (mux B ~E).
EXC_B eliminated (single excitation on PC6). All with appropriate
boot-state pull resistors.

### Gate driver: buck-dead fail-safe + zero boot glitch

Three design iterations:
1. **R_PA8_PU from V3V3:** Q_DRIVER biased ON via 3.3V rail. Boot
   glitch ~1ms (acceptable). BUT: if buck dies while 24V present,
   all four safety layers (MCU, IWDG, TPL5010, SCR latch) die with
   3.3V. R_GATE_PULL charges Q1 gate to 10V → heater ON indefinitely.
   Safety regression vs v1.0.
2. **Direct 24V divider on Q_DRIVER gate (PA8 connected):** Fixes
   the buck-dead scenario but PA8's ESD protection diode clamps the
   divider node below Vgs(th) when V3V3 = 0V, defeating the fail-safe.
3. **Q_CTRL isolation (current design):** PA8 controls Q_CTRL (2N7002),
   whose drain connects to Q_DRIVER gate. PA8 never sees the 24V
   divider voltage. Buck-dead: Q_CTRL gate at 0V (R_CTRL_PD pulls
   down), Q_DRIVER stays ON from 24V divider, heater OFF. Boot glitch:
   zero (resistive divider responds instantly). Logic polarity restored
   to match v1.0 (PA8 HIGH = heater ON). Sleep: 28µA from 24V
   (+4.2µA vs R_PA8_PU design, from the 4.7M/1M divider).

---

## Firmware Notes (from external review)

### LSE drive strength (LSEDRV)

The STM32G0B1 RTC_CR register includes LSEDRV[1:0] bits that control
the LSE oscillator drive current. Default is lowest drive (0b00).
At cold temperatures (<-20°C) and with the 32.768 kHz crystal's
increased ESR, the oscillator may fail to start or take >2 seconds.

**Action:** Set LSEDRV = 0b11 (highest drive) during LSE init.
Measure startup time on first boards across temperature range. If
startup is reliable at 0b10 or 0b01, reduce drive to save ~0.1 uA.

### Flash erase scheduling

W25Q256 sector erases (4 KB) take up to 400 ms. Page programs take
up to 3 ms. During erase, SPI is busy and cannot be interrupted.

**Action:** Schedule flash erases during heater-off periods (the
30-minute inter-measurement gap). Never erase during a measurement
scan. Use the W25Q256's suspend/resume feature if an urgent ADC
read is needed mid-erase.

### ADC health monitoring

The ADS1115 ALERT/DRDY pin can signal conversion-ready. If a
conversion doesn't complete within the expected window (2x the
nominal conversion time), the firmware should:

1. Log the timeout to flash
2. Reset the ADS1115 (I2C general-call reset, 0x06)
3. Retry the conversion
4. If 3 consecutive failures: flag the channel as faulted, skip it,
   report via RS-485

### EEPROM write protection

PA12 controls CAT24C32 WP pin (active high = protected). Boot-safe:
10k pull-up holds WP HIGH during reset, blocking all EEPROM writes.
Firmware must drive PA12 LOW before writing config/calibration data,
then drive HIGH immediately after. This prevents accidental EEPROM
corruption during brown-outs or firmware crashes.

---

## Open Items

None. All design items resolved.
