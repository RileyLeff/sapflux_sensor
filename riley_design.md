# Sap Flux Sensor Adapter Board

## Bill of Materials

24V input, 3.3V regulated output, heater driver. For JLCPCB PCBA.

### Protection

These parts sit between the 24V power input and the rest of the board. They protect the circuit from common real-world electrical hazards.

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| F_IN | Mini Blade Fuse Holder | C19727305 | XFCN XF-508P-B-B | User-installed mini automotive blade fuse | $0.27 | Extended | Single upstream fuse before the TVS/reverse-protection block. Protects the board and heater path. Use a low-current fuse for bring-up, then size for heater current after validation. |
| D2 | Bidirectional TVS | C19077549 | hongjiacheng SMAJ33CA | 33V standoff, ~53.3V clamp, SMA | $0.03 | **Preferred** | Clamps voltage spikes on the fused raw input. Bidirectional package removes D2 polarity/orientation risk. |
| Q_REV | Reverse Protection P-FET | C51025867 | HUASHUO HSU0195A | 100V, 17mΩ@10V, TO-252 | $1.04 | Extended | High-side PMOS reverse-polarity blocker. Drain to fused input, source to protected rail. Much lower loss than a series Schottky. |
| D_GS | Gate Clamp Zener | C19077459 | hongjiacheng MM3Z10 | 10V zener, SOD-323 | $0.01 | Extended | Clamps Q_REV Vgs so a 24V/29V battery does not exceed the MOSFET gate rating. |
| R_GS | Gate Pull Resistor | C23163 | UNI-ROYAL 0603WAF4704T5E | 4.7MΩ, 0603, 1% | $0.001 | Extended | Pulls Q_REV gate low in normal polarity. Adds only a few microamps of input-protection idle current. |
| F_BUCK | Buck PTC Fuse | C315903 | Littelfuse 2920L110/60MR | 1.1A hold, 60V, 2920 | $0.21 | Extended | Local/resettable buck-branch overcurrent protection. Can remain as redundant branch protection after the new main input fuse. |
| D_BUCK | Buck Schottky | C16015 | MDD SS36 | 60V/3A, SMA | $0.04 | Extended | Local buck-branch reverse-current protection. No longer the whole-board reverse-polarity element. |

### Buck Converter (24V to 3.3V)

LM5165Y ultra-low Iq synchronous buck, fixed 3.3V output. PFM mode (RT shorted to GND) for 10.5µA sleep current. Design follows [TI Design 2](https://www.ti.com/lit/ds/snvsa47a/snvsa47a.pdf) (Section 8.2.2, Figure 50, page 26).

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| U_BUCK | Buck Regulator | C2071924 | TI LM5165YDRCR | 3-65V in, 150mA, fixed 3.3V, 10.5µA Iq, VSON-10 | $1.07 | Extended | Synchronous buck. No external freewheeling diode or feedback divider needed. |
| L1 | Inductor | C5289353 | XR XRNR4018-68uH/M | 68µH, 470mA Isat, 1Ω DCR, 4x4mm shielded | $0.036 | Extended | 68µH for margin on minimum on-time constraint (LF(min) = 30V x 180ns / 180mA = 30µH). Worst-case 54.4µH at -20% tolerance still clears 30µH. Shielded. |
| C1 | Input Cap | C29823 | Fenghua 1206B475K500NT | 4.7µF 50V X7R, 1206 | $0.04 | **Basic** | Local energy reservoir for buck switching pulses. Note: X7R DC bias derating at 24V may reduce effective capacitance to ~2µF. Acceptable for LM5165's low current draw (<75mA) in PFM mode. |
| C_VIN | VIN HF Decoupling | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | $0.001 | **Basic** | Place tight to LM5165 VIN/GND pins. Handles HF switching edges that the 4.7µF C1 is too slow for. Same part as bypass caps. |
| C2 | Output Cap | C76634 | TDK C2012X5R1A226KT000N | 22µF 10V X5R, 0805 | $0.03 | Extended | Smooths 3.3V output ripple. TI Design 2 uses 10µF; 22µF is conservative. |
| C6 | VCC Decoupling | C15849 | Samsung CL10A105KB8NNNC | 1µF 50V X5R, 0603 | $0.006 | **Basic** | Decouples U_BUCK's internal VCC LDO (pin 10). |
| R_ILIM | Current Limit | C2930080 | FOJAN FRC0603F2492TS | 24.9kΩ 1%, 0603 | $0.005 | Extended | ILIM pin to GND. Sets 180mA peak current / 75mA max PFM output for extra 3.3V rail margin while staying in PFM mode. |

**Pin connections:** RT → GND (PFM mode, no resistor). EN → VIN (always on; internal UVLO at 2.9V is sufficient since the data logger controls power — no need for an external EN divider). BST → SW (internal bootstrap, no external cap needed — P-FET high side). SS → open (internal 900µs soft-start). HYS → open. PGOOD → MCU GPIO via 10kΩ pull-up (see MCU section).

### Analog-to-Digital Converters

2x ADS1115 (16-bit I2C, 4-ch each) for 6 thermistor channels. Single-shot mode: 0.5µA idle. Internal reference. Shares I2C bus with EEPROM.

**PGA setting: +/-4.096V (all channels).** The default +/-2.048V FSR saturates on thermistor channels below ~15°C (10kΩ NTC voltage exceeds 2.048V as resistance rises with cold) and cannot read the ~3.2V excitation voltage on AIN3. At +/-4.096V (LSB = 125µV), worst-case temperature resolution is 0.009°C/count at 70°C — still far exceeding the ~0.01°C needed for sap flux measurements. Note: ADS1115 input voltage is capped by VDD (3.3V) regardless of PGA setting.

**East30 sensor specs** (see `east30docs/` for datasheets and QA certs):
- Thermistors: 10kΩ NTC at 25°C, North Star Sensors Curve 44 (Steinhart-Hart 4-coeff, 0-70°C range)
- Reference resistors: 6x 10kΩ, internal to probe (half-bridge dividers built into sensor assembly)
- Heater resistance: ~40-44Ω (measured per-unit on QA cert)
- Cable length: 2m per QA cert, but design supports arbitrary lengths (25m+). All protection, filtering, and signal integrity choices assume long field cables.
- Wiring: 1 excitation (red), 6 thermistor sense wires (color-coded per needle/position), 2 heater (black), 1 ground

**Ratiometric compensation:** The ADS1115 uses a fixed internal reference (2.048V), not the excitation rail. Any drift in the 3.3V excitation (thermal drift, load regulation, PFM ripple) becomes measurement error. Fix: route the filtered excitation voltage (after R_EXC/C_EXC) through R_AIN_EXC (1kΩ series protection + C_AIN_EXC filter, same as thermistor channels) into a spare ADC channel (U_ADC1 AIN3). Firmware reads excitation alongside thermistors and divides out drift, making the system pseudo-ratiometric.

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| U_ADC1 | 16-bit ADC | C37593 | TI ADS1115IDGSR | 16-bit, 4-ch, I2C, VSSOP-10 | $1.08 | Extended | Channels 1-3 + excitation sense (AIN3). ADDR→GND = 0x48. ALERT/DRDY → MCU GPIO (conversion-ready interrupt). |
| U_ADC2 | 16-bit ADC | C37593 | TI ADS1115IDGSR | 16-bit, 4-ch, I2C, VSSOP-10 | $1.08 | Extended | Channels 4-6. ADDR→VDD = 0x49. ALERT/DRDY → MCU GPIO. Same setup fee as U_ADC1. |
| R_ALRT1 | ADC1 ALERT Pull-up | — | — | 10kΩ 1%, 0402 | — | — | *Included in ADS1115 registry package.* ALERT/DRDY open-drain pull-up to 3.3V. |
| R_ALRT2 | ADC2 ALERT Pull-up | — | — | 10kΩ 1%, 0402 | — | — | *Included in ADS1115 registry package.* Same for U_ADC2. |

### EEPROM

Calibration, config, device identity. Written rarely. NOT used for heater safety state.

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| U_EEPROM | I2C EEPROM | C81193 | onsemi CAT24C32WI-GT3 | 32Kbit (4KB), I2C, SOIC-8 | $0.18 | Extended | 1M write cycle endurance. Shares I2C bus. Standby ~2µA. A0/A1/A2 → GND = address 0x50 (no conflict with ADCs at 0x48/0x49). |

### MCU

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| U_MCU | Microcontroller | C2847904 | ST STM32G0B1CBT6 | ARM Cortex-M0+, 64MHz, 128KB flash, 144KB RAM, LQFP-48 | $2.22 | Extended | 44 GPIOs, 2x I2C, 6x USART, 12x timer channels, 12-bit ADC. 1.7-3.6V. |
| R_BOOT0 | BOOT0 Pull-down | C25804 | UNI-ROYAL 0603WAF1002T5E | 10kΩ 1%, 0603 | $0.001 | **Basic** | BOOT0 to GND. Ensures normal boot from flash. TP_BOOT0 test pad allows pulling high for system bootloader access. Same part as R4. |

**MCU peripheral allocation** (pin assignment deferred to PCB layout phase, will use STM32CubeMX):
- **Power:** 4x 100nF on VDD/VDDA pins, FB_VDDA (600Ω ferrite bead) + 1µF bulk on VDDA, 100nF on NRST
- **I2C:** SDA + SCL (shared bus: 2x ADC + EEPROM, 5kΩ effective pull-ups from ADS1115 registry packages)
- **USART + RS-485 control:** TX, RX, DE, ~RE (4 pins). SP3485 receiver always on (~RE=0 via R_RE_PD pull-down). USART wakeup from Stop mode on RX start bit detection. R_DE_PD (100kΩ) pulls DE low during boot (prevents bus driving). Split DE/~RE enables firmware-configurable polling mode.
- **Timer output:** One channel for heater gate via R_GATE (one-pulse mode)
- **Watchdog I/O:** WAKE (EXTI input) + DONE (output) for TPL5010. RSTn → NRST.
- **GPIOs:** 3x LED (R/G/B sink), 1x PGOOD input, 2x ADC ALERT/DRDY inputs (conversion-ready interrupts, one per ADS1115)
- **SWD:** SWDIO + SWCLK to test pads
- **BOOT0:** R_BOOT0 (10kΩ) to GND, routed to TP_BOOT0
- **Excitation:** MCU GPIO (push-pull) through R_EXC/C_EXC filter to J2 Pin 4. GPIO-driven allows cutting ~1mA excitation current during sleep. Ratiometric measurement on AIN3 compensates for GPIO output voltage variation.

### RS-485 Transceiver

Converts MCU UART to differential RS-485 on the M8 cable. Half-duplex. Receiver always on for USART wakeup from Stop mode — logger can reach the adapter instantly via RS-485. SP3485 fail-safe receiver outputs clean idle when bus is floating (no bias resistors needed).

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| U_RS485 | RS-485 Transceiver | C8963 | MaxLinear SP3485EN-L/TR | 3.3V, half-duplex, 10Mbps, SOIC-8 | $0.20 | **Basic** | DE and ~RE on separate MCU GPIOs. Default mode: receive (DE=0/~RE=0, 800µA typ / 1500µA max per datasheet). Transmit: DE=1/~RE=1. Shutdown (DE=0/~RE=1) available for firmware polling mode. Fail-safe receiver: outputs HIGH (UART idle) when bus is floating — no external bias needed. |
| R_DE_PD | DE Default Pull-down | C25803 | UNI-ROYAL 0603WAF1003T5E | 100kΩ 1%, 0603 | $0.001 | **Basic** | DE pin to GND. STM32 GPIOs are Hi-Z during reset/boot — without this, DE floats and SP3485 may drive the bus. 100kΩ is high enough to not interfere with MCU drive. Same part as R_WD_PU. |
| R_RE_PD | ~RE Default Pull-down | C25803 | UNI-ROYAL 0603WAF1003T5E | 100kΩ 1%, 0603 | $0.001 | **Basic** | ~RE pin to GND. Ensures receiver is enabled during boot (DE=0/~RE=0 = receive). SP3485 fail-safe outputs idle when bus is floating, so enabling the receiver during boot is safe — MCU UART sees clean silence. Same part as R_WD_PU. |
| R_TERM | RS-485 Termination | C22787 | UNI-ROYAL 0603WAF1200T5E | 120Ω 1%, 0603 | $0.001 | **Basic** | In series with C_TERM (AC termination). Hard-populated: this adapter is always at end-of-line (point-to-point cable from logger). Terminates signal edges for data integrity but blocks DC current. |
| C_TERM | AC Termination Cap | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V X7R, 0603 | $0.001 | **Basic** | In series with R_TERM. Blocks DC path through termination. RC = 120Ω × 100nF = 12µs. At 115200 baud, |Z| = 123Ω (near-ideal 120Ω match). Same part as bypass caps — eliminates one unique part number. |
| D_RS485 | RS-485 ESD/TVS | C7420375 | hongjiacheng SM712 | Asymmetric 7V/12V standoff, 16V/26V clamp, dual-channel, SOT-23 | $0.07 | **Preferred** | Purpose-built for RS-485. Asymmetric standoff matches RS-485 common-mode range (-7V to +12V). SP3485 bus pin abs max is ±15V — SM712 clamps at 20V/26V which exceeds this, but this is standard RS-485 TVS practice (microsecond transients, not sustained). IEC 61000-4-2/4-5. Place near J1. Replaces PESD1CAN (clamped at 70V — useless, SP3485 dies at 15V). |

**RS-485 architecture:** No bias resistors. SP3485 has a fail-safe receiver — when the bus is floating (nobody transmitting), the receiver outputs logic HIGH (UART idle/mark). This eliminates the need for bias resistors, which in traditional designs create unpredictable DC current paths through the cable depending on the far-end device's termination and bias network. The receiver stays on during MCU Stop mode (800µA typ / 1500µA max from 3.3V = 130-243µA from 24V battery via buck). STM32 USART wakeup detects a start bit on RX and wakes the MCU from Stop — the logger can reach the adapter instantly by sending any RS-485 data. No polling latency, no wake line, no extra connector pins.

**RS-485 power modes (firmware-configurable, see Firmware Notes):** Default mode is always-on receiver (~130-243µA from battery). For large-scale deployments where per-unit battery cost matters, firmware can switch to polling mode (~15-20µA) by disabling the receiver between configurable listen windows. Hardware supports both modes via split DE/~RE pins — no PCB change needed.

### Heater Driver

MCU fires a 24V heater through an N-channel MOSFET for 1-2 second pulses (~0.6A). An external watchdog (TPL5010) with SCR latch provides MCU-independent kill capability — even if the STM32 die fails with GPIO latched high, the heater is permanently shut off within ~10 seconds and stays locked out until manual power cycle.

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| Q1 | N-FET (heater) | C48928121 | XCH XCHS3N12 | 120V/3A, 118mΩ@10V, Vgs(th)=1.8V, SOT-23 | $0.030 | Extended | 120V Vds gives 67V margin above TVS clamp (53.3V). Estimated ~645mΩ@3.3V (conservative) → 381mV drop, 225mW at 0.6A — well within 1.5W SOT-23 limit. Heater still gets 97% of ideal power. |
| D4 | Flyback Schottky | C16015 | MDD SS36 | 60V/3A, SMA | $0.04 | Extended | Clamps inductive kick from heater cable (design supports 25m+). 60V/3A handles worst-case stored energy. Same part as D1. |
| R_GATE | Series Gate Resistor | C21190 | UNI-ROYAL 0603WAF1001T5E | 1kΩ 1%, 0603 | $0.001 | **Basic** | Between MCU timer output and Q1 gate. Limits current so watchdog N-FET can overpower a stuck MCU GPIO. Also damps gate ringing. |
| R4 | Gate Pulldown | C25804 | UNI-ROYAL 0603WAF1002T5E | 10kΩ 1%, 0603 | $0.001 | **Basic** | Keeps heater OFF during MCU reset/boot. |
| C4 | Bulk Decoupling | C4054386 | Honor Elec VP1H101M0607 | 100µF 50V electrolytic, D6.3xL7.7mm | $0.05 | Extended | Absorbs heater switching transients locally. 50V rating gives proper margin for 29V+ solar rail (was 35V, only 6V margin — now 21V margin). Same footprint as old part. |
| U_WDT | External Watchdog | C473912 | TI TPL5010DDCR | 35nA, SOT-23-6, 1.8-5.5V | $0.36 | Extended | 9.6s watchdog timeout. Sends periodic WAKE to MCU; if MCU fails to assert DONE, RSTn goes low to kill heater and reset MCU. |
| R_DELAY | Timeout Resistor | C25950 | UNI-ROYAL 0603WAF1102T5E | 11kΩ 1%, 0603 | $0.001 | **Preferred** | Sets TPL5010 watchdog period to ~9.6 seconds via DELAY pin. |
| Q2_INV | PNP Inverter | C8542 | CJ SS8550 | PNP, SOT-23, hFE≥200 | $0.013 | **Basic** | Inverts RSTn (active-low) to drive N-FET gate clamp high on fault. |
| Q_WD | N-FET Gate Clamp | C8545 | CJ 2N7002 | 60V/115mA, SOT-23 | $0.014 | **Basic** | When PNP drives gate high, clamps Q1 gate to GND through R_GATE (1kΩ divider → Q1 gate ≤156mV). |
| R_BASE | PNP Base Resistor | C25804 | UNI-ROYAL 0603WAF1002T5E | 10kΩ 1%, 0603 | $0.001 | **Basic** | Limits PNP base current to ~0.21mA. Same part as R4. |
| D_ISO | Latch Isolation Diode | C2128 | CJ 1N4148WS | 100V/150mA, SOD-323 | $0.009 | **Basic** | In series with R_BASE (anode at PNP base, cathode at RSTn). When latched, reverse-biased → RSTn stays 3.3V → MCU boots. Without this, Q_LATCH pulls RSTn to 0.48V via R_BASE/R_WD_PU divider, holding MCU in permanent reset. 29× margin at -40°C (hFE=50). |
| R_WD_PD | N-FET Gate Pulldown | C25804 | UNI-ROYAL 0603WAF1002T5E | 10kΩ 1%, 0603 | $0.001 | **Basic** | Holds Q_WD gate low when PNP is off (normal operation). Same part as R4. |
| R_WD_PU | RSTn Pull-up | C25803 | UNI-ROYAL 0603WAF1003T5E | 100kΩ 1%, 0603 | $0.001 | **Basic** | Pulls TPL5010 RSTn to 3.3V when open-drain output is high-Z (normal). |
| Q_LATCH | NPN Latch | C20526 | CJ MMBT3904 | 40V/200mA, hFE≥100, SOT-23 | $0.010 | **Basic** | SCR latch: cross-coupled with Q2_INV (PNP). Collector to PNP base node, emitter to GND. One RSTn pulse triggers permanent heater lockout until power cycle. D_ISO (1N4148WS) isolates latch from RSTn/NRST — MCU still boots and reports fault. |
| R_FB | Latch Feedback | C25803 | UNI-ROYAL 0603WAF1003T5E | 100kΩ 1%, 0603 | $0.001 | **Basic** | Q2_INV collector → Q_LATCH base. Provides positive feedback to sustain latch. 24µA base drive gives 8× margin over required 0.31mA collector sink. Same part as R_WD_PU. |
| R_LATCH_PD | Latch Base Pulldown | C22935 | UNI-ROYAL 0603WAF1004T5E | 1MΩ 1%, 0603 | $0.001 | **Basic** | Q_LATCH base to GND. Prevents noise from triggering latch when PNP is off. 1MΩ ≫ R_FB (100kΩ) so it doesn't interfere with trigger. |
| R_PNP_BASE | PNP Base-Emitter Pull-up | C25803 | UNI-ROYAL 0603WAF1003T5E | 100kΩ 1%, 0603 | $0.001 | **Basic** | PNP base to 3.3V (emitter). Defines OFF state when D_ISO is reverse-biased and Q_LATCH is off. Without this, PNP base floats → leakage/noise could cause spurious heater lockout. 140× margin over transistor leakage at 85°C. Same part as R_WD_PU. |
| Q_ARM | Latch Arm FET | C8545 | CJ 2N7002 | 60V/115mA, SOT-23 | $0.014 | **Basic** | N-FET in series with R_FB. Breaks latch feedback loop until PGOOD charges arm_node. Prevents TPL5010 power-on reset (~60ms) from triggering permanent heater lockout at every boot. Same part as Q_WD. |
| R_ARM | Arm Delay Resistor | C22935 | UNI-ROYAL 0603WAF1004T5E | 1MΩ 1%, 0603 | $0.001 | **Basic** | PGOOD → arm_node charge resistor. τ = 1MΩ × 1µF = 1s. Arming delay ≥361ms (worst-case Vgs(th)=1.0V) vs 100ms max TPL5010 tRST = 3.6× margin. Same part as R_LATCH_PD. |
| C_ARM | Arm Delay Cap | C15849 | Samsung CL10A105KB8NNNC | 1µF 50V X5R, 0603 | $0.003 | **Basic** | Holds arm_node low during power-on, preventing latch feedback until RC charges past Q_ARM Vgs(th). Same part as C_EXC. |
| R_DONE_PD | DONE Pull-down | C25803 | UNI-ROYAL 0603WAF1003T5E | 100kΩ 1%, 0603 | $0.001 | **Basic** | TPL5010 DONE pin to GND. Keeps DONE low when MCU is in reset/unpowered, preventing floating input from confusing the watchdog. Same part as R_WD_PU. |
| C_WD | Watchdog Bypass | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | $0.001 | **Basic** | TPL5010 VDD to GND decoupling. Same part as other 100nF caps. |

**Heater safety — 4-layer architecture (see Firmware Architecture for details):**
1. **Hardware timer:** STM32 one-pulse mode enforces max pulse duration in silicon.
2. **IWDG watchdog:** Independent RC oscillator resets MCU if firmware hangs → R4 pulls gate low.
3. **External watchdog (TPL5010):** MCU-independent. If MCU fails to pet within ~9.6s, RSTn goes low → PNP inverts → N-FET clamps Q1 gate to GND. Works even if STM32 die is completely dead.
4. **Thermal interlock:** Firmware reads all 6 thermistors before every firing. Refuses if any needle is elevated.

**Gate-kill circuit with SCR latch:**
```
MCU timer ──→ R_GATE (1kΩ) ──→ Q1 gate ──→ R4 (10kΩ) ──→ GND
                                   ↑
                              Q_WD drain
                              Q_WD source → GND
                              Q_WD gate ←── Q2_INV collector ──→ R_FB (100kΩ) ──→ Q_ARM ──→ Q_LATCH base
                                              ↑                                     ↑            ↑
                                        R_WD_PD (10kΩ) → GND                  arm_node    R_LATCH_PD (1MΩ) → GND
                                        Q2_INV emitter → 3.3V                              Q_LATCH emitter → GND
                                        Q2_INV base ←── R_BASE (10kΩ) ←── D_ISO ←── RSTn
                                              ↑  ↑           (anode)    (cathode)      ↑
                                              |  R_PNP_BASE (100kΩ) → 3.3V
                                        Q_LATCH collector                        R_WD_PU (100kΩ) → 3.3V
                                                                                 TPL5010 RSTn (open-drain)
                                                                                 MCU NRST

Latch arm circuit:
  PGOOD → R_ARM (1MΩ) → arm_node → C_ARM (1µF) → GND
                            ↑
                       Q_ARM gate
```
Power-on: PGOOD LOW → arm_node at 0V → Q_ARM OFF → R_FB can't reach Q_LATCH → latch disabled. PNP still activates during TPL5010 POR → Q_WD clamps gate (heater off during reset, good). After ~60ms, TPL5010 releases RSTn → PNP off → heater gate free. Then PGOOD goes HIGH → RC charges → after ≥361ms, Q_ARM turns on → latch armed.
Normal: RSTn HIGH → D_ISO reverse-biased → R_PNP_BASE holds PNP base at 3.3V → PNP OFF → collector ~0V → NPN OFF → MCU controls Q1 freely. Zero power. Q_ARM is on (armed) but no current flows.
Fault: RSTn LOW → D_ISO forward-biased → PNP ON → collector HIGH → R_FB → Q_ARM (on) → Q_LATCH ON → pulls PNP base LOW → **latched permanently**. D_ISO reverse-biased when latched → RSTn stays HIGH → MCU NRST not held low — MCU boots and reports fault via RS-485. Heater locked out until power cycle.
Latch power: 0.64mA from 3.3V (0.1mA from 24V battery). Only during fault — irrelevant.
RSTn also connects to MCU NRST for belt-and-suspenders reset.

**PGOOD-gated arming is critical:** Without the arm gate, the TPL5010 power-on reset (RSTn LOW for ~60ms at every boot, per datasheet section 7.3.1) would trigger the SCR latch, permanently locking out the heater before the MCU even boots. Q_ARM (N-FET in R_FB path) breaks the feedback loop until PGOOD charges arm_node through a 1MΩ/1µF RC (τ=1s). Worst-case arming delay is 361ms (Vgs(th)=1.0V) vs 100ms max tRST — 3.6× safety margin. All three arm components (2N7002, 1MΩ, 1µF) use existing BOM part numbers.

**D_ISO is critical:** Without the isolation diode, Q_LATCH (when latched) pulls the PNP base to 0.2V. R_BASE (10kΩ) forms a voltage divider with R_WD_PU (100kΩ) → RSTn = 0.48V, which is below the STM32 NRST LOW threshold (~1.1V). The MCU would be stuck in permanent reset and unable to report the fault. D_ISO (1N4148WS) blocks this reverse current path. At -40°C with hFE=50, trigger still has 29× margin.

**TPL5010 RSTn + SCR latch:** The TPL5010 RSTn output is a pulse (~60ms typ, 100ms max), not sustained. The cross-coupled PNP/NPN latch (Q2_INV + Q_LATCH) converts this pulse into a permanent hold: one RSTn LOW pulse triggers the latch (when armed), which keeps the PNP saturated and Q_WD gate clamped indefinitely — even after RSTn returns HIGH. The latch can only be cleared by a power cycle (removing battery/solar). D_ISO (1N4148WS, in series with R_BASE) isolates the latch from the RSTn/NRST net: when latched, D_ISO is reverse-biased, so the MCU still resets and boots normally — it can report the fault via RS-485, but cannot fire the heater until manually power-cycled. This is intentional: TPL5010 only fires when layers 1 (timer) and 2 (IWDG) have both failed, indicating a serious fault. Permanent lockout is the appropriate response.

### ADC Input Protection

The 6 thermistor lines and excitation sense line travel through potentially long cable runs (25m+, via J2) to the ADC inputs. Each line is protected by a PRTR5V0U4D TVS array at the connector side (clamps ESD transients before they reach any downstream components), followed by a 1kΩ series resistor and 100nF shunt cap forming an RC low-pass filter (fc = 1.6kHz) that dumps HF noise from long cables to ground before it reaches ADC silicon. TVS placement at the connector side is critical — placing the 1kΩ resistor before the TVS would allow multi-kV ESD voltages to develop across the 0603 resistor before the TVS can clamp.

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| R_AIN1-R_AIN6 | Series Resistors (x6) | C21190 | UNI-ROYAL 0603WAF1001T5E | 1kΩ 1%, 0603 | $0.001 ea | **Basic** | One per thermistor ADC input, between J2 and ADS1115 AINx pins. Limits current into ADS1115's internal ESD clamp diodes during transients. 1kΩ adds negligible error given the thermistor divider's ~5kΩ source impedance. |
| R_AIN_EXC | Excitation Sense Resistor | C21190 | UNI-ROYAL 0603WAF1001T5E | 1kΩ 1%, 0603 | $0.001 | **Basic** | Series protection on excitation sense line to U_ADC1 AIN3. Same part as R_AIN1-6. Without this, transients on the excitation trace go straight into the ADC pin. |
| C_AIN1-C_AIN6 | Input Filter Caps (x6) | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | $0.001 ea | **Basic** | After each R_AIN, to GND. Forms 1kΩ + 100nF low-pass (fc = 1.6kHz). Essential for 25m+ cable runs — hardware anti-aliasing and HF EMI dump. Same part as bypass caps. |
| C_AIN_EXC | Excitation Sense Filter | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | $0.001 | **Basic** | After R_AIN_EXC, to GND. Matches filter on thermistor channels. Same part as bypass caps. |
| D_J2A | Sensor TVS Array (x1) | C2835429 | TECH PUBLIC PRTR5V0U4D-TP | 4-ch ESD, 5.5V standoff, 300W, 0.3pF, SOT-23-6 | $0.09 | Extended | Rail-clamp ESD array on excitation + data 1-3 (4 channels). VCC to 3.3V, GND to GND. Clamps transients from 25m+ outdoor cables. 0.3pF adds negligible capacitance vs. the 100nF filter caps. Place near J2. IEC 61000-4-2/4-5/4-4. |
| D_J2B | Sensor TVS Array (x1) | C2835429 | TECH PUBLIC PRTR5V0U4D-TP | 4-ch ESD, 5.5V standoff, 300W, 0.3pF, SOT-23-6 | $0.09 | Extended | Same part for data 4-6 (3 of 4 channels used). Unused 4th channel: tie I/O pin to GND per datasheet recommendation. Same setup fee as D_J2A. |

### I2C Bus

Shared bus: MCU, EEPROM, 2x ADC. Pull-ups provided by ADS1115 registry packages (2× 10kΩ per line = 5kΩ effective). No separate pull-up resistors needed. 5kΩ is within I2C fast mode spec (valid range: 967Ω–5.6kΩ at 3.3V, 50pF bus capacitance).

### Decoupling & Bypass Capacitors

Every IC needs a local bypass cap (100nF ceramic, placed as close to the power pins as possible). The MCU also needs a bulk cap on VDDA, a ferrite bead isolating VDDA from digital VDD, and a cap on NRST.

| Ref | Component | LCSC | MPN | Specs | Qty | Price ea | Type | Notes |
|-----|-----------|------|-----|-------|-----|----------|------|-------|
| C_MCU1-C_MCU4 | MCU VDD bypass | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | 4 | $0.001 | **Basic** | One per VDD pin (STM32G0B1 has 3x VDD + 1x VDDA). |
| FB_VDDA | VDDA Ferrite Bead | C1002 | Sunlord GZ1608D601TF | 600Ω@100MHz, 200mA, 450mΩ, 0603 | 1 | $0.013 | **Basic** | In series between 3.3V rail and VDDA pin. Attenuates digital switching noise (~MHz) from reaching the analog supply. 200mA is ample for VDDA current (ADC + internal ref, <1mA). 450mΩ DCR → <0.5mV drop. Place between 3.3V plane via and C_VDDA/C_MCU4 landing pads. |
| C_VDDA | MCU VDDA bulk | C15849 | Samsung CL10A105KB8NNNC | 1µF 50V X5R, 0603 | 1 | $0.006 | **Basic** | Additional bulk cap on VDDA for analog reference stability. On VDDA side of FB_VDDA (after ferrite bead). Same part as C6. |
| C_NRST | MCU Reset filter | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | 1 | $0.001 | **Basic** | NRST pin to GND. Filters noise on reset line. |
| C_EEPROM | EEPROM bypass | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | 1 | $0.001 | **Basic** | U_EEPROM VCC to GND. |
| C_ADC1 | ADC1 bypass | — | — | 100nF, 0402 | 1 | — | — | *Included in ADS1115 registry package.* U_ADC1 VDD to GND. |
| C_ADC2 | ADC2 bypass | — | — | 100nF, 0402 | 1 | — | — | *Included in ADS1115 registry package.* U_ADC2 VDD to GND. |
| C_RS485 | RS-485 bypass | — | — | 100nF, 0402 | 1 | — | — | *Included in SP3485 registry package.* U_RS485 VCC to GND. |

Explicit bypass caps use C14663 (already in BOM, basic, no additional setup fee). Total: 6 instances here (C_MCU1-4 + C_NRST + C_EEPROM) + C5 (shield) + C_WD (watchdog bypass) + C_VIN (buck HF) + C_AIN1-6 (6x ADC filter) + C_AIN_EXC (excitation filter) + C_TERM (AC termination) + C_ESD_A + C_ESD_B (TVS local decoupling) = 19 total instances of C14663. C_ADC1, C_ADC2, and C_RS485 are now provided by atopile registry packages (0402, auto-picked). Note: C3 (bootstrap) removed — LM5165Y uses internal bootstrap (P-FET high side).

### PGOOD Monitoring

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| R_PGOOD | PGOOD Pull-up | C25804 | UNI-ROYAL 0603WAF1002T5E | 10kΩ 1%, 0603 | $0.001 | **Basic** | LM5165 PGOOD is open-drain. Pull-up to 3.3V lets MCU read power-good status. Firmware can delay I2C init until 3.3V is confirmed stable. Same part as R4. |

### Thermistor Excitation Filter

The ADS1115 uses an internal reference (not ratiometric), so any noise on the excitation driving the thermistor dividers directly becomes measurement error. An RC filter cleans up noise before it reaches the thermistors. R_EXC is 560Ω — low enough for acceptable DC drop with ratiometric compensation (2.52V worst-case at 85°C), but high enough to limit GPIO inrush current to 5.8mA when charging C_EXC from 0V (100Ω would draw 33mA, exceeding STM32's 20mA per-pin spec).

**GPIO-driven excitation:** Excitation is sourced from an MCU GPIO (push-pull, ~3.25V at 1mA) rather than the 3.3V rail directly. This allows cutting ~1mA excitation current during sleep by setting the GPIO low before entering STOP2. The ratiometric measurement (AIN3) compensates for any GPIO output voltage variation. On wake: set GPIO high → wait ~3ms for C_EXC to charge (5× RC at 560Ω × 1µF) → read ADCs.

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| R_EXC | Excitation Filter R | C23204 | UNI-ROYAL 0603WAF5600T5E | 560Ω 1%, 0603 | $0.001 | **Basic** | 560Ω limits GPIO inrush to 5.8mA (100Ω drew 33mA — exceeds STM32 20mA spec). DC drop at 1mA = 0.56V (excitation ~2.7V at 25°C). Ratiometric measurement on AIN3 compensates. fc = 284Hz with C_EXC — even better buck ripple attenuation than 100Ω (1.6kHz). |
| C_EXC | Excitation Filter C | C15849 | Samsung CL10A105KB8NNNC | 1µF 50V X5R, 0603 | $0.006 | **Basic** | Shunt cap after R_EXC. RC = 560Ω × 1µF = 560µs, fc = 284Hz. LM5165 PFM switches at ~100kHz+ so this gives ~51dB attenuation (vs ~36dB at old 100Ω). Same part as C6/C_VDDA. |

### Connectors

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| J1 | M8 Receptacle | C19108968 | CAZN M8-P4A-GPL M8-JD | 4-pin, A-coded, male, right-angle, TH | $2.95 | Extended | Upstream to data logger. IP67 when mated. **Hand-solder.** |
| J2 | Pluggable Header | C441318 | KF2EDGV-3.5-10P | 10-pin, 3.5mm pitch, vertical, TH | $0.12 | Extended | Downstream to sensor probe. **Hand-solder.** |
| - | Pluggable Plug | C440844 | KF2EDGK-3.5-10P | 10-pin, cable-side plug | $0.44 | - | **Not a PCB part** — buy separately. |
| C5 | Shield Cap | C14663 | YAGEO CC0603KRX7R9BB104 | 100nF 50V, 0603 | $0.001 | **Basic** | J1 shell to GND. Blocks ground loop DC, passes HF EMI. |

### Status LED

Common-anode RGB. MCU sinks current through each color via a resistor.

| Ref | Component | LCSC | MPN | Specs | Price | Type | Notes |
|-----|-----------|------|-----|-------|-------|------|-------|
| LED1 | RGB LED | C22461780 | XINGLIGHT XL-B1212RGBA-HF | 1.2x1.2mm, common anode, SMD-4P | $0.007 | Extended | Anode to 3.3V. R/G/B cathodes to MCU GPIOs via resistors. |
| R_R | LED Red | C23228 | UNI-ROYAL 0603WAF6800T5E | 680Ω 1%, 0603 | $0.001 | **Basic** | (3.3-2.1V)/680Ω = 1.8mA. Note: modern SMD LEDs are very bright at 2mA — may be blinding indoors but appropriate for outdoor/sunlight visibility. Increase to 2-4kΩ if indoor use is primary. |
| R_G | LED Green | C8218 | UNI-ROYAL 0603WAF2000T5E | 200Ω 1%, 0603 | $0.001 | **Basic** | (3.3-2.9V)/200Ω = 2mA. |
| R_B | LED Blue | C22962 | UNI-ROYAL 0603WAF2200T5E | 220Ω 1%, 0603 | $0.001 | **Basic** | (3.3-2.8V)/220Ω = 2.3mA. |

### Debug & Test

| Ref | Component | Specs | Notes |
|-----|-----------|-------|-------|
| TP_SWDIO | Test Pad | Through-hole pad | SWD data. No header needed — touch debug probe during development. |
| TP_SWCLK | Test Pad | Through-hole pad | SWD clock. |
| TP_GND | Test Pad | Through-hole pad | Ground reference for debug probe. |
| TP_3V3 | Test Pad | Through-hole pad | 3.3V for target detection. |
| TP_SDA | Test Pad | Through-hole pad | I2C data — useful for bus debugging with scope/logic analyzer. |
| TP_SCL | Test Pad | Through-hole pad | I2C clock. |
| TP_HTR | Test Pad | Through-hole pad | Heater gate drive signal. |
| TP_BOOT0 | Test Pad | Through-hole pad | STM32 BOOT0 pin. Pull high to enter system bootloader if SWD is locked out or firmware is bricked. Lifesaver during development. |
| TP_24V_PROT | Test Pad | Through-hole pad | Protected 24V rail (after F1/D2/D1). Verify protection circuit output and measure voltage under load. |
| TP_RS485_A | Test Pad | Through-hole pad | RS-485 A line near U_RS485. Scope probe point for signal integrity, eye diagrams, and termination tuning. |
| TP_RS485_B | Test Pad | Through-hole pad | RS-485 B line near U_RS485. Differential probe with TP_RS485_A. |

Test pads are just exposed copper pads on the PCB — zero BOM cost, zero assembly cost. Include during layout.

### M8 Pinout (J1)

| Pin | Signal | Wire Color | Notes |
|-----|--------|------------|-------|
| 1 | 24V | Brown | Power input |
| 2 | RS485 A (+) | White | Non-inverted data |
| 3 | GND | Blue | Supply return |
| 4 | RS485 B (-) | Black | Inverted data |
| 5-6 | Shell | - | Through C5 (100nF) to GND |

### 10-Pin Sensor Pinout (J2)

| Pin | Signal | Notes |
|-----|--------|-------|
| 1 | Heater A | 24V protected rail (heater element is non-polar) |
| 2 | Heater B | Low-side switched return via Q1 (heater element is non-polar) |
| 3 | Ground | Sensor ground reference |
| 4 | Excitation | Filtered 3.3V for thermistor dividers (through R_EXC/C_EXC) |
| 5-10 | Data 1-6 | Thermistor analog lines (through R_AIN1-6 to ADC inputs) |

### BOM Summary

- **Basic/preferred parts (no setup fee):** C29823, C14663, C25804, C8963, C23228, C8218, C22962, C23206, C23204, C22787, C21190, C15849, C8542, C8545, C25803, C25950, C7420375, C1002, C20526, C22935, C2128 (21 unique). Note: I2C pull-ups (was C23162), ADC/RS-485 decoupling, and ALERT pull-ups are now provided by atopile registry packages (0402 passives, auto-picked at build time).
- **Extended parts ($3 setup fee each):** C315903, C16015, C693627, C2071924, C5289353, C76634, C48928121, C4054386, C37593, C81193, C22461780, C19108968, C441318, C473912, C2847904, C2835429 (16 unique × $3 = $48 setup for first run)
- **Hand-solder:** J1, J2 (through-hole, easy)
- **Buy separately:** KF2EDGK-3.5-10P cable-side plug
- **Action items:**
  - Import: `just add C2071924` (LM5165Y), `just add C4054386` (100µF 50V)
  - Import: `just add C8963` (SP3485), `just add C22461780` (RGB LED), `just add C7420375` (SM712 RS-485 TVS)
  - Import: `just add C2835429` (PRTR5V0U4D-TP sensor TVS), `just add C1002` (ferrite bead VDDA)
  - Import: `just add C20526` (MMBT3904 NPN latch), `just add C22935` (1MΩ latch pulldown), `just add C2128` (1N4148WS latch isolation diode)
  - Remove old/unused parts from library: LMR14006YDDCR (C477919), SS34 (C8678), 33.2k (C23003), C3339 (old 35V cap), AO3422 (old 55V MOSFET), C22775 (100Ω, replaced by C23204 560Ω), C57112 (10nF, replaced by C14663 100nF)
  - Place all new components on sap_adapter sheet
  - Fix pin types on imported symbols (unspecified → passive) in Symbol Editor
- **Local datasheets** (`datasheets/`, gitignored — download from LCSC `wmsc` URLs):
  - `LM5165Y.pdf` — Buck converter (TI, 37 pages)
  - `STM32G0B1CBT6.pdf` — MCU (ST, ~1000 pages)
  - `ADS1115.pdf` — ADC (TI, 40 pages)
  - `SP3485.pdf` — RS-485 transceiver (MaxLinear, 9 pages)
  - `TPL5010.pdf` — Watchdog timer (TI, 10 pages)
  - `CAT24C32.pdf` — EEPROM (onsemi, 16 pages)
  - `PRTR5V0U4D.pdf` — ESD TVS array (Tech Public, 4 pages)
  - `SM712.pdf` — RS-485 TVS (hongjiacheng, 3 pages)

---

## Design Documentation

### Power

#### Connector

4-pin M8 A-coded connector (J1). PCB side has male pins so the cable is female-to-female — exposed cable ends can't accidentally short.

**Pinout** standardized by IEC 61076-2-104 / IEC 60947-5-2:

| Pin | Color  | Our Assignment | Convention |
|-----|--------|----------------|------------|
| 1   | Brown  | 24V (V+)       | Supply positive |
| 2   | White  | RS-485 A+      | Signal |
| 3   | Blue   | GND (0V)       | Supply return |
| 4   | Black  | RS-485 B-      | Signal |

Watch out: black is **not** ground. In the M8 world, blue is ground.

**Shield capacitor (C5, 100nF 50V):** AC-couples the M8 housing to GND. Shunts high-frequency EMI while blocking DC ground loop current on long cable runs.

#### Protection Circuit (F_IN, D2, Q_REV)

```
CN1+ --> [F_IN mini blade fuse] --> V24_FUSED_RAW --> [Q_REV PMOS] --> +24V Protected
                                      |
                                  [D2 bidirectional TVS]
                                      |
                                     GND
```

**F_IN (mini blade input fuse):** Main upstream fuse. Protects the whole board, including the heater path, and gives D2 an upstream opening device if the TVS ever fails short. Fuse value is user-selectable: start conservative for bring-up, then size for validated heater current.

There is intentionally no second heater-branch fuse. The heater copper is routed as part of the protected 24V distribution, so duplicating a high-current fuse downstream adds routing congestion and extra hot joints without isolating many realistic faults. For bring-up, install a small fuse first. For full-power validation, choose the value around the measured heater current and wiring limit; 8 heaters at 50Ω from 30V would be 4.8A if all were energized together, so a 7.5A mini blade fuse is a practical full-load lab value after low-current checks pass.

**D2 (SMAJ33CA bidirectional TVS, 33V standoff, ~53V clamp):** Parallel voltage clamp on the fused raw input. Bidirectional TVS removes the polarity-sensitive assembly failure mode from v1.1.

**Q_REV (P-channel MOSFET):** Series high-side reverse-polarity protection. Drain faces the fused input, source feeds the protected rail, and a 10V zener clamps gate-source voltage. Normal idle draw from the gate network is only a few microamps.

### Firmware Architecture

#### Vision
Async `no_std` Rust library (Embassy on STM32G0):

```rust
sensor.read_temperature(Channel::UpperTip).await   // -> Result<Temperature, SapFluxError>
sensor.fire_heater(Duration::from_secs(2)).await    // -> Result<(), SapFluxError::TooHot>
sensor.conduct_measurement(config).await            // -> Result<SapFluxData, SapFluxError>
```

The MCU earns its keep as a safety interlock and protocol handler. Main loop must service TPL5010 WAKE interrupts by asserting DONE (rising edge) within the ~9.6s watchdog period — this is the heartbeat that keeps the external watchdog from killing the heater path and resetting the MCU.

#### State Management

**RAM (volatile):** Measurement state, sample buffers, session counters. Should reset on power loss.

**EEPROM (persistent, rare writes):** Calibration coefficients, device ID, channel mapping, config presets, RS-485 node ID, RX power mode (always-on vs polling).

**Thermistors as physical state (heater safety):** On every heater request, MCU reads all 6 thermistors. If any needle is elevated above ambient (>0.5°C above reference channels), request is refused with `TooHot`. Also flags if any single needle diverges significantly from the others (e.g., one at 50°C while the rest are at 25°C) — indicates a stuck heater, sensor pulled from wood, or hardware fault. Measures physical reality rather than trusting a bookkeeping entry.

#### Firmware Notes

**TPL5010 sync:** Use EXTI interrupt on the WAKE pin. When the TPL5010 pulses WAKE, assert DONE immediately in the ISR. Don't use a free-running timer — the TPL5010's RC oscillator drifts relative to the STM32's clock, so syncing to WAKE keeps the heartbeat aligned.

**I2C bus arbitration:** ADCs and EEPROM share the bus. EEPROM page writes block for ~5ms. Do not start an EEPROM write during a time-critical ADC measurement sequence. Sequence: (1) perform all ADC reads, (2) then write EEPROM if needed. Embassy's async I2C driver handles bus-busy naturally, but the application must order operations correctly.

**Ratiometric ADC reads:** For each measurement cycle, read U_ADC1 AIN3 (excitation voltage) alongside the 6 thermistor channels. Compute resistance as R_therm = R_ref × (V_ain / V_exc) / (1 - V_ain / V_exc). This cancels excitation drift (including GPIO output voltage variation) and improves absolute accuracy. All channels use PGA = +/-4.096V (see ADC section for rationale).

**Sleep entry sequence:** (1) Turn off excitation (excitation GPIO low). (2) Enter Stop mode. SP3485 receiver stays on (~RE=0). USART remains configured for wakeup on RX start bit. GPIOs maintain state during Stop. In polling mode: firmware sets ~RE=1 before Stop, wakes on timer, briefly enables receiver (~RE=0) to listen.

**Wake/measurement sequence:** (1) Turn on excitation GPIO. (2) Wait ~3ms for C_EXC to charge (5× RC at 560Ω). (3) Read all thermistor channels + excitation on AIN3. (4) If comms needed: set DE=1 to transmit, DE=0 to listen. RS-485 receiver is already on — no enable step needed.

**ADC data rate:** Configure ADS1115 for low SPS (8 or 16 SPS) rather than the default 128 SPS. The 1kΩ series protection resistors + 100nF filter caps + long cable capacitance (design supports 25m+) create a high-impedance source. Lower data rates give the ADS1115's internal sampling capacitor more time to settle, improving accuracy.

**ADS1115 mux settling:** The 1kΩ series resistors + cable capacitance create channel-to-channel memory effects on the ADS1115's muxed front end. After switching channels, perform a dummy conversion and discard the result before taking the real measurement. This adds ~125ms per channel at 8 SPS but eliminates inexplicable inter-channel offsets.

**ADS1115 ALERT/DRDY:** Use the ALERT/DRDY pin (configured as conversion-ready) on each ADC to signal completion via MCU EXTI interrupt. This lets the MCU sleep (WFI) during the 125ms conversion instead of polling the I2C bus, saving power and freeing the bus for other devices.

**USART wakeup from Stop:** The STM32G0 USART peripheral can detect a start bit on the RX pin and wake the MCU from Stop mode. In Embassy, this is the natural async pattern: the executor enters Stop when all tasks are awaiting, and a UART interrupt wakes it. The main loop uses `select()` between `uart_rx.read()` (wakes on RS-485 activity) and a measurement timer (wakes on schedule). No polling, no latency for incoming commands.

**No ADC reads during heater pulse:** With 25m cables, 0.6A heater current through the shared ground wire creates ~800mV ground bounce (22AWG = 53mΩ/m × 25m one-way = 1.32Ω, × 0.6A = 794mV). This shifts all thermistor readings by ~6,400 LSBs (at PGA ±4.096V). The sap flux measurement protocol naturally separates ADC reads (before/after pulse) from the heater pulse itself, but firmware must enforce this constraint: never call ADC read functions while the heater GPIO is active. After the heater pulse ends, wait 10ms before reading ADCs — cable inductance (~25µH for 25m) and parasitic capacitance (~2.5nF) ring at ~637kHz with L/R decay τ=38µs; 10ms gives >250τ for complete settling.

**Runtime configurable RX power mode:** Hardware supports two RS-485 listen modes via EEPROM config, with no PCB changes required (split DE/~RE pins enable both):
- **Always-on (default):** Receiver enabled (~RE=0). USART wakeup from Stop on RX start bit. ~130-243µA from 24V battery (800-1500µA from 3.3V via buck). Zero-latency response to logger commands. Works with any data logger out of the box.
- **Polling mode (advanced):** Receiver disabled (~RE=1) between listen windows. MCU sleeps for a configurable interval (1-60s), wakes briefly (~50ms) to listen, then sleeps again. ~15-20µA from 24V battery. Logger must retry or send a preamble longer than the poll interval. Useful for large-scale deployments (1000+ sensors) where per-unit battery cost dominates.

**Safety boot (anti-bricking):** Regardless of EEPROM RX power mode setting, the receiver is forced ON for 10 seconds after every power cycle. This prevents soft-bricking — if a user sets an aggressive poll interval that their logger can't accommodate, power-cycling the sensor opens a 10-second window to send a "reset config" command.

**Unused MCU pins:** STM32G0B1 has 44 GPIOs; this design uses ~19. Configure all unused pins as analog mode (GPIO_MODE_ANALOG) during initialization — this is the reset default and the lowest-leakage state. Avoid push-pull low which can sink current if a pin accidentally couples to a signal. Floating digital inputs waste power through CMOS input buffer shoot-through and can pick up noise that couples into the analog section.

**Sleep current budget (always-on receiver mode):**
| Source | 3.3V draw | Notes |
|--------|-----------|-------|
| LM5165 Iq | — | 10.5µA from 24V (not from 3.3V rail) |
| STM32G0B1 Stop2 | ~2µA | 3.3V, all clocks off, RAM retained |
| SP3485 RX (DE=0, ~RE=0) | 800µA typ / 1500µA max | Dominates 3.3V budget |
| 2× ADS1115 (power-down) | 1µA total | Single-shot mode, both idle |
| CAT24C32 standby | 2µA | |
| TPL5010 | — | 35nA from 3.3V (negligible) |
| I2C pull-ups (idle) | 0µA | Bus idle = high, no current through pull-ups |
| R_WD_PU, R_DE_PD, R_RE_PD | <1µA total | 100kΩ to rail, negligible |
| **Total from 3.3V** | **~805µA typ** | |
| **Total from 24V** | **~27µA typ** | Via buck (η≈85% at light load, Iq=10.5µA) |

**Q1 MOSFET second-source note:** XCHS3N12 (C48928121) is single-source from XCH. For production runs, evaluate alternatives like ZXMN10A07F (Diodes Inc, 100V/1.4A, SOT-23) or AO3416 (Alpha & Omega, 100V/2.3A, SOT-23) as drop-in replacements. Key specs: Vds≥100V, Vgs(th)≤2.5V, SOT-23, Rds(on)≤1Ω@3.3V.

#### Heater Safety Layers

Designed for unattended deployment in dry/dead trees. At 14.4W (24V into ~40Ω needle), napkin math shows:
- 10s → ~171°C needle temp (safe)
- 30s → ~232°C (smoldering risk)
- 60s → ~264°C (pilot ignition risk)

Four independent layers, any one of which can prevent a fire:

1. **Hardware timer (primary):** STM32 timer in one-pulse mode. CPU triggers the pulse; timer peripheral autonomously enforces max duration. Even if CPU hangs mid-pulse, the timer hardware stops the output on schedule.
2. **Independent watchdog (secondary):** IWDG clocked from independent RC oscillator. If firmware stops petting it, MCU resets. On reset, GPIOs go high-Z, R4 pulls Q1 gate low.
3. **External watchdog + SCR latch (tertiary — MCU-independent):** TPL5010 with ~9.6s hardware timeout (R_DELAY=11kΩ). MCU must assert DONE after each WAKE pulse. If MCU fails (firmware hang, die failure, GPIO latched high), TPL5010 asserts RSTn LOW → triggers cross-coupled PNP/NPN latch → Q1 gate permanently clamped to GND until power cycle. D_ISO (1N4148WS) isolates latch from NRST — MCU boots, reports fault via RS-485, but cannot fire heater. 35nA quiescent — essentially free. Latch adds ~$0.04 BOM (1× MMBT3904 + 1× 1N4148WS + 2× resistors).
4. **Thermal interlock (application layer):** Thermistor check before every firing. No persisted state dependency. Measures physical reality — can't be fooled by stale software state.

### Project Context

#### What this board does
Adapter PCB between an East30 sap flux sensor probe (6 thermistor channels + heater) and a field data logger:
- 24V power + RS-485 from data logger via M8 cable
- 3.3V buck for MCU and analog front-end
- 6 thermistor channels via 2x ADS1115 (16-bit I2C ADCs)
- 24V heater driver (~0.6A, 1-2 second pulses) with hardware safety timeout
- I2C EEPROM for calibration/config
- Must survive outdoor deployment: solar charging (29V+), lightning, ESD, arbitrary cable lengths (25m+)
- **Operating temperature:** -40°C to +85°C (all critical ICs are industrial grade: STM32G0B1 -40/+85°C, ADS1115 -40/+125°C, LM5165 -40/+125°C, SP3485 -40/+85°C)

### PCB Layout Guidelines
- **4-layer stackup:** Signal / GND / 3.3V / Signal. Dedicated inner ground plane (layer 2) directly below top signal layer gives the best return path for ADC traces and contains buck converter EMI. Inner 3.3V plane simplifies power distribution to MCU, ADCs, and peripherals.
- **Board dimensions:** Target ≤100×100mm (JLCPCB cost sweet spot). ~60×50mm is comfortable for 72 components on 4 layers.
- **Fiducials:** 3x global fiducial marks for JLCPCB pick-and-place alignment. 1mm copper circle with 2mm soldermask opening, placed asymmetrically near board corners.
- **Mounting holes:** Include M3 mounting holes; exact count and placement deferred to layout phase based on final board dimensions and enclosure selection.
- M8 and 10-pin header on opposite edges
- Buck components tight (short SW trace, CIN close to VIN)
- Heater MOSFET near J2
- MCU central, EEPROM nearby
- **Analog isolation:** ADC input traces short, away from buck/heater switching noise. R_AIN resistors and C_AIN filter caps close to J2. Excitation filter (R_EXC/C_EXC) near J2.
- **Trace widths:** 24V/heater ≥0.5mm. 3.3V/signal 0.25mm. GND plane for returns.
- **Thermal relief:** Use spoked thermal relief on M8 connector (J1) GND pins. The connector body is a large thermal slug — without relief, hand-soldering requires excessive heat that can damage the connector or PCB.
- **GND return isolation:** Heater GND return (0.6A pulsed) must not route through the analog ground area under the ADCs. Keep heater power loop (Q1 source → GND plane → J2 GND → heater → Q1 drain) physically separated from the sensitive ADC input area. Star-ground or split-plane approach near ADCs. **J2 ground pin trace split:** J2 pin 3 (GND) carries both 0.6A heater return and ADC ground reference. Split the trace at the connector pad into two paths — a fat trace (≥0.5mm) to the heater power loop area, and a separate quiet trace to the analog ground region — before they merge at the ground plane.
- **D4 flyback diode:** Place near J2 (sensor connector), not near Q1. For 25m+ cables, the inductive energy is in the cable — clamping is most effective at the connector where the cable attaches.
- **C_VIN:** Place 100nF ceramic as close to LM5165 VIN/GND pins as possible, shorter loop than C1. Handles HF switching transients.
- Test pads accessible from board edge
- **D_J2A/D_J2B local decoupling:** Place a 100nF cap (C14663) near the VCC pin of each PRTR5V0U4D TVS array, with short traces to the 3.3V and GND planes. During ESD events, the rail-clamp arrays dump surge current into the 3.3V rail — the local cap absorbs this charge and prevents the rail from jumping.
- **Ground pour on L4 under buck:** Add a copper fill (stitched to L2 GND plane) on the bottom signal layer under the LM5165 and inductor area. Provides additional thermal dissipation and EMI shielding for the switching node.
- JLCPCB 4-layer specs: min 0.127mm trace/clearance, 0.3mm via drill, 0.2mm annular ring
