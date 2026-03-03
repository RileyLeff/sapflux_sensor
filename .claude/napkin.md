# Napkin

## Corrections
| Date | Source | What Went Wrong | What To Do Instead |
|------|--------|----------------|-------------------|
| 2026-03-02 | prior session | Called `jlc_search_api` which doesn't exist as an MCP tool | Use `jlc_search` (the fast DB search) instead — `jlc_search_api` is not a valid tool name |
| 2026-03-02 | prior session | v1.1 plan said "D1 stays as SS36 — it only sees input current" | D1 is in the SERIES path (protection.ato confirms: J1→PTC→TVS→D1→V24_PROT). D1 carries ALL current including heater. Must trace actual topology, not assume. |
| 2026-03-02 | prior session | Proposed 15N10-TD MOSFET (Vgs(th)=2.5V) for 3.3V gate drive | At 3.3V gate, only 0.8V overdrive → multi-ohm Rds(on). Always check Vgs(th) vs actual gate voltage. Resolved with gate driver circuit (10V from 24V rail). |
| 2026-03-02 | self | Assigned PD4, PD5, PC8 to mux enables + EXC_B — none exist on LQFP-48 | STM32G0B1 LQFP-48 has ONLY: PC6,PC7,PC13-15 and PD0-PD3. No PC0-5, PC8-12, PD4+. Always verify pin existence against package definition, not datasheet's full-chip pin list. |
| 2026-03-02 | self | Gate driver narrative said "heater OFF during boot" — actually ON | N-ch low-side pull-down topology is inherently inverting. Default state (driver off) = MOSFET gate pulled HIGH by R_GATE_PULL = heater ON. Always trace the boot state (all GPIOs high-Z) through the full circuit. |
| 2026-03-02 | self | Spare GPIO list included PC0, PC1, PC2 — don't exist on LQFP-48 | Same root cause: assumed PC0-2 existed without checking package. |
| 2026-03-03 | self | Connected PA8 directly to 24V divider node (Q_DRIVER gate). When V3V3=0V, STM32 ESD clamp forward-biases to VDD, sinking divider current into dead rail. Q_DRIVER gate pulled below Vth — fail-safe defeated. | MCU GPIO pins have internal ESD diodes to VDD+0.6V. Never connect a GPIO to a node biased above VDD when VDD may be 0V. Use an isolation MOSFET (Q_CTRL) between the GPIO and the higher-voltage node. |
| 2026-03-03 | self | Assumed thermistor R25=44kΩ because "Curve 44". Carried wrong value through sensitivity + noise calculations for entire session. | Curve 44 = 10,000Ω at 25°C. The "44" is a curve NUMBER. Always check the actual Steinhart-Hart coefficient sheet. Source: `east30docs/Steinhart-HartCoefficientsByCurve (1).pdf`. |

## User Preferences
- All math in Python. Never mental-math. User rule.
- Atomic commits — one logical change per commit.
- User wants anti-footgun designs (safe by default, hard to misuse).
- User is OK with dual/triple-row connectors. Not married to single-row.
- User wants explanations of EE concepts when asked — teach, don't just prescribe.

## Patterns That Work
- Tracing actual signal paths through .ato files to verify topology claims
- Running Python calculations for every circuit parameter
- Searching JLCPCB parts with `jlc_search` (fast DB). Use spec_filters with Unicode units (e.g., "4.7MΩ" not "4.7M").
- Cross-referencing design doc against atopile implementation for discrepancies

## Patterns That Don't Work
- Trusting prior design doc claims without verifying against .ato source
- Assuming MOSFET Rds(on) specs at 10V apply at 3.3V gate drive
- `jlc_search_api` MCP tool — does not exist

## Domain Notes
- This is a sap flux sensor adapter board — scientific instrument, field-deployed
- v1.1 scales from 1 to 8 heater probes (parallel), 32-48 thermistor channels (2-3x 74HC4067 mux)
- Thermistors: North Star Sensors Curve 44 = **10kΩ @ 25°C** (NOT 44kΩ). Steinhart-Hart 4-coeff model.
- Probe topology: EXC → R_EXC (270Ω) → cable → R_ref (10k, in probe) → sense_node → R_therm (NTC) → GND. Mux reads sense_node.
- Excitation depression: 48 parallel dividers load excitation line. V_exc drops from 3.3V to ~2.0V at 48ch/25°C through R_EXC. Ratiometric measurement cancels this — firmware MUST read V_exc, not assume 3.3V (20°C error otherwise).
- Gate driver circuit: Q_DRIVER (2N7002) + Q_CTRL (2N7002) + 10V zener + divider. Non-inverted logic (PA8 HIGH = heater ON). Zero boot glitch. Buck-dead fail-safe: 24V divider holds Q_DRIVER ON independent of 3.3V.
- LQFP-48 pin availability is LIMITED. Always check the ato package file, not assumptions. Key gaps: no PC0-5, no PC8-12, no PD4+.
- Boot-state analysis: trace every new IC's control pins through MCU high-Z state. Add pull-ups/pull-downs for defined boot behavior (mux ~E, flash CS, etc.).
- ADS1115 speed/noise Pareto: 64-250 SPS sweet spot for derivative estimation
- Adaptive scan: 860/250/64 SPS for fast/medium/slow thermal phases
