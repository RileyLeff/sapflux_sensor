# Post-Build Script Plan

Run these steps after every `ato build` to restore manually-added PCB elements that atopile wipes.

## Steps

1. **`ato build`** — rebuild from atopile source (run from `elec/` directory)

2. **Fix passive silkscreen** — move R/C/L/D references from F.SilkS to F.Fab
   ```
   python3 elec/fix_passive_layers.py
   ```

3. **Fix via drill sizes** — Power_24V netclass vias get 0.4mm drill, need 0.3mm
   - Find all vias with `(drill 0.4)` inside `(via ...)` blocks
   - Replace with `(drill 0.3)`

4. **Insert mounting holes** — 4× M3 (3.2mm NPTH)
   - (104, 89) — top-left, 4mm inset
   - (196, 89) — top-right
   - (104, 146) — bottom-left
   - (196, 146) — bottom-right
   - Bolt pattern: 92mm × 57mm

5. **Insert tooling holes** — 3× JLCPCB (1.152mm NPTH, 0.148mm solder mask expansion)
   - (101.5, 86.5) — top-left, 1.5mm inset
   - (198.5, 86.5) — top-right
   - (101.5, 148.5) — bottom-left

6. **Insert test pads** — 8× through-hole (1.5mm pad, 1mm drill)
   - Positions and net assignments captured at commit 37e4e1b
   - TP_V3V3 (net 85), TP_GND (net 75), TP_V24 (net 179), TP_GATE (net 96)
   - TP_PGOOD (net 97), TP_NRST (net 21), TP_PNP (net 15), TP_EXC (net 63)
   - Note: net IDs may change after rebuild — match by net name instead

7. **Re-apply silkscreen positions** — restore custom label offsets for ICs/connectors/test pads
   - Captured at commit 37e4e1b (see conversation history for full position dump)

8. **Board art** — NOT automated. Logos and decorative graphics on F.SilkS/B.SilkS/F.Mask must be re-applied manually from SVG assets in `assets/`.

## Notes

- Board edge: (100, 85) to (200, 150) — 100mm × 65mm
- All vias: 0.5mm outer / 0.3mm drill (JLCPCB POFV ≤0.5mm requirement)
- Board is shippable as of 2026-03-19. This script only needed if future `ato build` is required.
- Python snippets for all steps exist in Claude conversation history — consolidate into a single `post_build.py` when needed.
