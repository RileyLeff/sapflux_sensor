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
| 2026-03-05 | self | Old netclass patterns referenced v1.0 net names (V24_PROT, node_a, thermistor_*) that don't exist in v1.1 | After major topology changes, audit ALL netclass patterns against actual PCB net names. Run `grep 'net \d' default.kicad_pcb` to get the real list. |
| 2026-06-05 | self | In Blender bpy, hand-built bmesh box used as a boolean cutter silently no-op'd (EXACT solver removed nothing) because its face normals were inconsistent. Cylinders from `primitive_cylinder_add` worked, so only box-shaped cuts failed — easy to miss. | After building a box with `bmesh.faces.new`, ALWAYS call `bmesh.ops.recalc_face_normals(bm, faces=bm.faces[:])` before `to_mesh`. Verify booleans with `obj.ray_cast` (window centre should be OPEN, a known-solid spot SOLID) — never trust a Workbench render to confirm through-holes. |
| 2026-06-06 | self | Enclosure standoff posts printed as detached islands (connected only to supports, fell off the build plate). Root cause: a concentric bore WIDER than the post severs it from its body plate — lid counterbore r3.75 > pillar r3.0 (0.75mm ring gap), base nut-pocket r3.3 > post r3.0 (0.3mm ring). A Workbench/exploded render looked fine; the gap is sub-mm. | When a post has a counterbore/pocket cut into its root, ALWAYS check bore radius vs post radius — if bore > post, the post is severed. Reconnect OUTBOARD (flare the post past the bore, "volcano") only where layout allows; near windows reconnect INBOARD (leave a web: don't bore through the full plate). VERIFY with a connected-component (shell) count on the mesh AND on the exported STL: a one-piece part = 1 shell; each floating post adds one. ray_cast/manifold checks alone won't catch a clean island. |
| 2026-06-05 | self | Imported a KiCad glb (metres) into Blender and hard-coded a x1000 m->mm scale, but I'd set `scene.unit_settings.scale_length=0.001` first, so the glTF importer ALREADY pre-scaled to mm — the x1000 blew the board up to 100,000mm and off-screen. | The glTF importer respects scene unit scale. Don't assume metres. Measure the imported bbox (board is 100mm wide) and pick factor = 1000 if span<10 else 1. KiCad glb keeps X, NEGATES Y, board bottom ~z0. **Do NOT correct the Y-negation with `scaleY<0` — a negative single-axis scale is a REFLECTION (mirror), not a rotation: it flips silkscreen backwards and mirrors every cut → caused the first-print mirror bug.** Use a POSITIVE scale and reflect through POSITION instead: `scale=(f,f,f)`, `location=(-100, BOARD_H+85, Z_BOARD_BOT)` → `by_scene = 150 - Yk` (board in true orientation, silkscreen reads forward). Confirm with a top-down render that reads silkscreen text. |

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
- Imported LCSC parts with bare pins need `signal p1 ~ pin 1` etc. added manually
- Imported LCSC parts with wrong designator prefix need prefix trait updated (U→J for connectors, X→Y for crystal)
- Explicit component packages (not Resistor) can't use bridge connect (~>) — use explicit `comp.p1 ~ node_a` wiring
- TVS passthrough in protection.ato means V24_RAW = V24_PROT electrically — only one net name override allowed

## Patterns That Don't Work
- Trusting prior design doc claims without verifying against .ato source
- Assuming MOSFET Rds(on) specs at 10V apply at 3.3V gate drive
- `jlc_search_api` MCP tool — does not exist

## Domain Notes
- This is a sap flux sensor adapter board — scientific instrument, field-deployed
- v1.1: 48 thermistor channels (3x 74HC4067 mux), W25Q256 SPI flash, TM15N10D DPAK MOSFET with 10V gate driver, 60-pin thermistor connector, CR2032 VBAT backup, 32.768kHz RTC crystal
- Thermistors: North Star Sensors Curve 44 = **10kΩ @ 25°C** (NOT 44kΩ). Steinhart-Hart 4-coeff model.
- Probe topology: EXC → R_EXC (270Ω) → cable → R_ref (10k, in probe) → sense_node → R_therm (NTC) → GND. Mux reads sense_node.
- Excitation depression: 48 parallel dividers load excitation line. V_exc drops from 3.3V to ~2.0V at 48ch/25°C through R_EXC. Ratiometric measurement cancels this — firmware MUST read V_exc, not assume 3.3V (20°C error otherwise).
- Gate driver circuit: Q_DRIVER (2N7002) + Q_CTRL (2N7002) + 10V zener + divider. Non-inverted logic (PA8 HIGH = heater ON). Zero boot glitch. Buck-dead fail-safe: 24V divider holds Q_DRIVER ON independent of 3.3V.
- LQFP-48 pin availability is LIMITED. Always check the ato package file, not assumptions. Key gaps: no PC0-5, no PC8-12, no PD4+.
- Boot-state analysis: trace every new IC's control pins through MCU high-Z state. Add pull-ups/pull-downs for defined boot behavior (mux ~E, flash CS, etc.).
- ADS1115 speed/noise Pareto: 64-250 SPS sweet spot for derivative estimation
- Adaptive scan: 860/250/64 SPS for fast/medium/slow thermal phases
- M8 4-pin connector (C19108968, CAZN): rated **3A** per pin. 8 parallel heaters draw 4.8A — 60% over rating. M8 caps at 4A across all manufacturers (geometry-limited). M12 A-coded = 4A (marginal). M12 T-coded = 12A (real solution). Flag for future rev.
- Via annular ring: 0.6mm dia / 0.3mm drill = 0.15mm ring (1.5× JLCPCB min). Old 0.5mm/0.3mm was exactly at minimum.
- MOSFET thermal vias: 8× 0.4mm drill >> 6× 0.3mm drill (21.7°C vs 37.8°C ΔT through board at 1.57W)
