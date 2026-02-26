# Design Doc Update Plan

Pending changes to `riley_design.md` from review rounds 3-4 (Gemini + GPT) and RS-485 architecture redesign.

## Summary of Decisions

1. **RS-485 bias removal:** Remove R_BIAS_A, R_BIAS_B, Q_BIAS entirely. SP3485 fail-safe receiver handles floating bus (outputs idle/mark when bus is open). Bias resistors were legacy — created unpredictable power draw depending on far-end device wiring (0-5mA through cable depending on logger's termination). Removing them eliminates the entire class of problems.

2. **Receiver always on:** SP3485 stays in receive mode (DE=0, ~RE=0) during sleep instead of shutdown. Actual datasheet numbers (verified from Exar SP3485_100_061912 PDF): **800µA typ / 1500µA max** receiver-only current. Shutdown mode is **not specified** in the datasheet (the 10µA product page figure has no backing in the electrical characteristics table). At worst case 1500µA: 243µA from 24V battery → 37 months pure sleep on 7Ah. At typical 800µA: 130µA from 24V → 73 months. Both measured in years — irrelevant for a 24V battery/solar system.

3. **USART wakeup from Stop mode:** STM32G0 USART detects start bit on RX and wakes MCU from Stop. Embassy async pattern: `select(uart_rx.read(), measurement_timer.next())`. Logger can reach adapter instantly by sending RS-485 data — no polling, no latency, no wake line needed. This is the idiomatic pattern for low-power RS-485 sensors.

4. **Keep M8 4-pin connector.** No wake line needed (USART wakeup solves it). No connector change. No footgun risk from IO-Link M8 5-pin confusion.

5. **R_EXC 100ohm -> 560ohm (C22775 -> C23204):** GPT found real bug — GPIO inrush into C_EXC was 33mA, exceeding STM32's 20mA per-pin spec. At 560ohm: inrush = 5.8mA (safe), fc = 284Hz (better filtering), DC drop at 85C worst case = 0.73V (excitation 2.52V, fine with ratiometric). Settling time increases from 500us to 2.8ms. C23204 (560ohm) is already in BOM (was R_BIAS_A/B, now repurposed for R_EXC). C22775 (100ohm) removed from BOM entirely.

6. **C_TERM 10nF -> 100nF (C57112 -> C14663):** Better AC termination at 115200 baud (|Z|=123ohm vs 301ohm). Uses C14663 already in BOM for 20+ bypass caps. Removes C57112 as unique part.

7. **Add R_ALRT1/R_ALRT2:** ADS1115 ALERT/DRDY is open-drain, needs external pull-ups. 2x 10kohm (C25804, already in BOM). Missing component found by GPT.

8. **Add firmware note: no ADC during heater pulse.** Ground bounce on 25m cables = ~800mV at 0.6A. Sap flux method naturally separates ADC reads from heater pulse, but make constraint explicit.

9. **Add layout note: D_J2A/D_J2B local decoupling.** PRTR5V0U4D rail-clamp arrays dump surge current into 3.3V rail. Need local 100nF near TVS VCC pins (uses existing C14663, no BOM change — just an instance count and layout note).

## Section-by-Section Edit List

### 1. RS-485 Transceiver section (lines 80-98)

**Line 82 — section intro:**
- OLD: "Half-duplex with separate DE and ~RE pins for low-power shutdown."
- NEW: "Half-duplex. Receiver always on for USART wakeup from Stop mode — logger can reach the adapter instantly via RS-485. SP3485 fail-safe receiver outputs clean idle when bus is floating (no bias resistors needed)."

**Line 86 — U_RS485 row notes:**
- OLD: emphasis on shutdown mode (DE=0/~RE=1, 10uA) and separate pin requirement
- NEW: Default mode is receive (DE=0/~RE=0, 800µA typ / 1500µA max per datasheet). Transmit: DE=1/~RE=1. Shutdown (DE=0/~RE=1) possible but not used in normal operation — datasheet does not specify shutdown current. Separate DE/~RE still needed so DE can be held low during receive, and to support firmware-configurable polling mode.

**Line 87 — R_DE_PD row:**
- Keep as-is. Still needed to hold DE low during boot (prevents bus driving when GPIOs are Hi-Z).

**Line 88 — R_RE_PU -> R_RE_PD:**
- OLD: R_RE_PU, 100kohm pull-UP to 3.3V, "Ensures receiver is disabled during boot (DE=0/~RE=1 = shutdown)"
- NEW: R_RE_PD, 100kohm pull-DOWN to GND, "Ensures receiver is enabled during boot (DE=0/~RE=0 = receive). SP3485 fail-safe outputs idle when bus is floating, so enabling the receiver during boot is safe — MCU UART sees clean silence."

**Lines 89-91 — DELETE entirely:**
- R_BIAS_A row (C23204 560ohm)
- R_BIAS_B row (C23204 560ohm)
- Q_BIAS row (C8545 2N7002)

**Line 93 — C_TERM row:**
- OLD: C57112, 10nF 50V X7R, "RC = 120ohm x 10nF = 1.2us — adequate for 9600-115200 baud"
- NEW: C14663, 100nF 50V X7R, "RC = 120ohm x 100nF = 12us. At 115200 baud |Z|=123ohm (near-ideal 120ohm match). Same part as bypass caps — eliminates one unique part number."

**Lines 96-98 — Replace "RS-485 sleep power" and "Bus topology note":**
- DELETE both paragraphs
- NEW single paragraph: "**RS-485 architecture:** No bias resistors. SP3485 has a fail-safe receiver — when the bus is floating (nobody transmitting), the receiver outputs logic HIGH (UART idle/mark). This eliminates the need for bias resistors, which in traditional designs create unpredictable DC current paths through the cable depending on the far-end device's termination and bias network. The receiver stays on during MCU Stop mode (800µA typ / 1500µA max from 3.3V = 130-243µA from 24V battery). STM32 USART wakeup detects a start bit on RX and wakes the MCU from Stop — the logger can reach the adapter instantly by sending any RS-485 data. No polling latency, no wake line, no extra connector pins."

### 2. MCU peripheral allocation (lines 69-78)

**Line 72:**
- OLD: "USART + RS-485 control: TX, RX, DE, ~RE (4 pins — separate DE and ~RE enables SP3485 shutdown mode). R_DE_PD (100kohm) pulls DE low and R_RE_PU (100kohm) pulls ~RE high during boot -> default shutdown state."
- NEW: "USART + RS-485 control: TX, RX, DE, ~RE (4 pins). SP3485 receiver always on (~RE=0 via R_RE_PD pull-down). USART wakeup from Stop mode on RX start bit detection. R_DE_PD (100kohm) pulls DE low during boot (prevents bus driving)."

**Line 75:**
- OLD: "3x LED (R/G/B sink), 1x PGOOD input, 1x Q_BIAS enable (RS-485 bias switching), 2x ADC ALERT/DRDY inputs"
- NEW: "3x LED (R/G/B sink), 1x PGOOD input, 2x ADC ALERT/DRDY inputs" (remove Q_BIAS GPIO — freed up)

### 3. ADC section (lines 49-52)

**Add after U_ADC2 row (line 52), or in a nearby section:**
- NEW rows: R_ALRT1 and R_ALRT2, 10kohm pull-ups (C25804) on ADS1115 ALERT/DRDY open-drain outputs. One per ADC, to 3.3V. "ADS1115 ALERT/DRDY is open-drain — requires external pull-up for conversion-ready interrupt to work."

### 4. Excitation Filter section (lines 192-201)

**Line 194 — intro text:**
- OLD: "R_EXC must be low-value (100ohm) because 6 parallel thermistor dividers present only ~3.3kohm load at 25C."
- NEW: Update to reflect 560ohm. Explain GPIO inrush fix. "R_EXC is 560ohm — low enough for acceptable DC drop with ratiometric compensation (2.52V worst-case at 85C), but high enough to limit GPIO inrush current to 5.8mA when charging C_EXC from 0V (100ohm would draw 33mA, exceeding STM32's 20mA per-pin spec)."

**Line 196 — GPIO-driven excitation:**
- Update settling time: "wait ~3ms for C_EXC to charge (5x RC at 560ohm x 1uF)" (was 500us)

**Line 200 — R_EXC row:**
- OLD: C22775, 100ohm, "At 100ohm, drop is ~100mV (3.2V excitation). At 10kohm, excitation collapses to 0.82V"
- NEW: C23204, 560ohm, "560ohm limits GPIO inrush to 5.8mA (100ohm drew 33mA — exceeds STM32 20mA spec). DC drop at 1mA = 0.56V (excitation ~2.7V at 25C). Ratiometric measurement on AIN3 compensates. fc = 284Hz with C_EXC — even better buck ripple attenuation than 100ohm (1.6kHz)."

**Line 201 — C_EXC row:**
- Update RC calculation: "RC = 560ohm x 1uF = 560us, fc = 284Hz"

### 5. Firmware notes (lines 332-350)

**Line 340 — Sleep entry sequence:**
- OLD: "(1) Set SP3485 to shutdown (DE=0, ~RE=1). (2) Turn off Q_BIAS (bias GPIO low). (3) Turn off excitation (excitation GPIO low). (4) Enter STOP2."
- NEW: "(1) Turn off excitation (excitation GPIO low). (2) Enter Stop mode. SP3485 receiver stays on (~RE=0). USART remains configured for wakeup on RX start bit. GPIOs maintain state during Stop."

**Line 342 — Wake/measurement sequence:**
- OLD: "(1) Turn on excitation GPIO. (2) Wait ~500us ... (4) If heater firing needed: turn on Q_BIAS, enable SP3485 receiver..."
- NEW: "(1) Turn on excitation GPIO. (2) Wait ~3ms for C_EXC to charge (5x RC at 560ohm). (3) Read all thermistor channels + excitation on AIN3. (4) If comms needed: set DE=1 to transmit, DE=0 to listen. RS-485 receiver is already on — no enable step needed."

**Add new firmware note after line 348:**
- "**USART wakeup from Stop:** The STM32G0 USART peripheral can detect a start bit on the RX pin and wake the MCU from Stop mode. In Embassy, this is the natural async pattern: the executor enters Stop when all tasks are awaiting, and a UART interrupt wakes it. The main loop uses `select()` between `uart_rx.read()` (wakes on RS-485 activity) and a measurement timer (wakes on schedule). No polling, no latency for incoming commands."

**Add new firmware note:**
- "**No ADC reads during heater pulse:** With 25m cables, 0.6A heater current through the shared ground wire creates ~800mV ground bounce (at 22AWG). This shifts all thermistor readings by thousands of LSBs. The sap flux measurement protocol naturally separates ADC reads (before/after pulse) from the heater pulse itself, but firmware must enforce this constraint: never call ADC read functions while the heater GPIO is active."

**Add new firmware note:**
- "**Runtime configurable RX power mode:** Hardware supports two RS-485 listen modes via firmware config stored in EEPROM, with no PCB changes required (split DE/~RE pins enable both):
  - **Always-on (default):** Receiver enabled (~RE=0). USART wakeup from Stop on RX start bit. ~130-243µA from 24V battery. Zero-latency response to logger commands. Works with any data logger.
  - **Polling mode (advanced):** Receiver disabled (~RE=1) between listen windows. MCU sleeps for a configurable interval (1-60s), wakes briefly (~50ms) to listen, then sleeps again. ~15-20µA from 24V battery. Logger must retry or send a preamble longer than the poll interval. Useful for large-scale deployments (1000+ sensors) where per-unit battery cost dominates.

  **Safety boot:** Regardless of EEPROM setting, receiver is forced ON for 10 seconds after every power cycle. This prevents soft-bricking — if a user sets an aggressive poll interval that their logger can't accommodate, power-cycling the sensor opens a 10-second window to send a 'reset config' command."

### 6. Decoupling section — instance count (line 184)

- Update C14663 instance count: add 1 for C_TERM (now C14663 instead of C57112). Was 20 instances, now 21.
- Add layout note about D_J2A/D_J2B needing local 100nF near VCC pins (another C14663 instance, or just a layout directive to use nearby existing caps).

### 7. BOM Summary (lines 258-270)

**Basic/preferred list:**
- REMOVE: C57112 (10nF, was C_TERM — now using C14663)
- REMOVE: C22775 (100ohm, was R_EXC — now using C23204 560ohm)
- C23204 stays (560ohm, now used for R_EXC instead of bias)
- C8545 stays (2N7002, still used by Q_WD)
- Count: 21 -> 19 unique basic/preferred parts

**Extended list:**
- No change. 16 unique, $48 setup.

**Action items:**
- Remove `C22775` and `C57112` from any "import" lists (no longer needed)
- C23204 already imported (was for bias resistors, now for R_EXC)

### 8. Design Documentation — Power section (lines 276-293)

- No change needed. M8 4-pin stays. Pinout stays.

### 9. PCB Layout Guidelines (lines 378-394)

**Line 380:**
- Update component count estimate if mentioned (drops by ~3 parts)

**Add layout note:**
- "**D_J2A/D_J2B local decoupling:** Place a 100nF cap (C14663) near the VCC pin of each PRTR5V0U4D TVS array, with short traces to the 3.3V and GND planes. During ESD events, the rail-clamp arrays dump surge current into the 3.3V rail — the local cap absorbs this charge and prevents the rail from jumping."

### 10. Project Context (lines 366-376)

**Line 370:**
- "24V power + RS-485 from data logger via M8 cable" — no change needed

## Net BOM Impact

| Change | Parts Removed | Parts Added | Unique SKUs |
|--------|--------------|-------------|-------------|
| Remove bias (R_BIAS_A, R_BIAS_B, Q_BIAS) | 3 instances | 0 | C23204 stays (R_EXC uses it) |
| R_RE_PU -> R_RE_PD | 0 | 0 | Same part (C25803), just rewired |
| R_EXC 100ohm -> 560ohm | C22775 removed | C23204 used | -1 unique (C22775 gone) |
| C_TERM 10nF -> 100nF | C57112 removed | C14663 used | -1 unique (C57112 gone) |
| Add R_ALRT1/R_ALRT2 | 0 | 2x C25804 | 0 new (already in BOM) |
| **Totals** | 3 parts removed | 2 parts added | -2 unique SKUs |

**Final counts:** 19 unique basic/preferred, 16 unique extended ($48 setup).
**Net component count:** -1 (3 removed, 2 added).

## MEMORY.md Updates Needed

- Update RS-485 architecture description (no bias, receiver always on, USART wakeup)
- Remove Q_BIAS from architecture decisions
- Update current state (19 basic, 16 extended)
- Add note: R_EXC changed to 560ohm (GPIO inrush fix)
- Add note: C_TERM changed to 100nF (part consolidation)
- Add note: R_ALRT1/R_ALRT2 added for ADS1115 ALERT/DRDY
- Remove C22775 and C57112 from any part lists
