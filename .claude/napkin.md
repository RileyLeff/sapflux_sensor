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

## User Preferences
- All math in Python. Never mental-math. User rule.
- Atomic commits — one logical change per commit.
- User wants anti-footgun designs (safe by default, hard to misuse).
- User is OK with dual/triple-row connectors. Not married to single-row.
- User wants explanations of EE concepts when asked — teach, don't just prescribe.

## Patterns That Work
- Tracing actual signal paths through .ato files to verify topology claims
- Running Python calculations for every circuit parameter
- Searching JLCPCB parts with `jlc_search` (fast DB) and `jlc_search_api` → NO, use `jlc_search` only
- Cross-referencing design doc against atopile implementation for discrepancies

## Patterns That Don't Work
- Trusting prior design doc claims without verifying against .ato source
- Assuming MOSFET Rds(on) specs at 10V apply at 3.3V gate drive
- `jlc_search_api` MCP tool — does not exist

## Domain Notes
- This is a sap flux sensor adapter board — scientific instrument, field-deployed
- v1.1 scales from 1 to 8 heater probes (parallel), 32 thermistor channels (dual mux)
- Gate driver circuit: 2N7002 + 10V zener + 3 resistors. Inverted logic (PA8 LOW = heater ON). Brief heater-ON glitch (~2ms) at every boot — thermally negligible, documented and accepted.
- LQFP-48 pin availability is LIMITED. Always check the ato package file, not assumptions. Key gaps: no PC0-5, no PC8-12, no PD4+.
- Boot-state analysis: trace every new IC's control pins through MCU high-Z state. Add pull-ups/pull-downs for defined boot behavior (mux ~E, flash CS, etc.).
- ADS1115 speed/noise Pareto: 64-250 SPS sweet spot for derivative estimation
- Adaptive scan: 860/250/64 SPS for fast/medium/slow thermal phases
