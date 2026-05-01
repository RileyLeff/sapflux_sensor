# Sap Flux Adapter v1.1 Shipped Release

This directory freezes the fabrication package for the physical boards ordered
from JLCPCB in March 2026.

## Status

- Hardware status: fabricated and delivered.
- Canonical design source: `../../elec/main.ato` and the supporting Atopile
  modules in `../../elec/`.
- Canonical handcrafted layout: `../../elec/layouts/default/default.kicad_pcb`.
- Canonical fabrication package: the Gerbers, drill file, JLCPCB BOM, and
  JLCPCB CPL in this directory.

The live KiCad project is still named `default` because it was generated and
then hand-routed through the Atopile/KiCad workflow. The name is plain, but the
layout is important: it contains the hand placement, routing, board art, and
late manufacturing fixes that were sent to JLCPCB.

## Why This Package Is Canonical

Email records with JLCPCB confirm that order `SMT026032160870` included the
remark `moved R19 to avoid collision + fixed Bat1 footprint issue`. JLCPCB
asked whether to proceed, Riley confirmed production, then approved the final
placement/polarity review. The shipped item was listed as `gerbers_Y3` under
JLCPCB item `W2026032010337545`.

The files here are copied from `../../elec/layouts/default/gerbers/`, whose
Gerber/drill files were generated on 2026-03-21. The older root
`../../gerber_export/` files were generated on 2026-03-10 and should be treated
as superseded.

## Contents

- `default.kicad_pcb` - KiCad PCB layout snapshot.
- `default.kicad_pro` - KiCad project settings snapshot.
- `main.ato` - Atopile top-level module snapshot.
- `ato.yaml` - Atopile build configuration snapshot.
- `gerbers/gerbers.zip` - JLCPCB fabrication archive.
- `gerbers/jlcpcb-bom.csv` - JLCPCB assembly BOM.
- `gerbers/jlcpcb-cpl.csv` - JLCPCB pick-and-place file.
- `gerbers/default.drl` - Drill file.
- `SHA256SUMS.txt` - Checksums for all files in this release.

## Tool Versions

- Atopile: `0.14.1004`
- KiCad PCB generator recorded in `default.kicad_pcb`: `pcbnew` `10.0`

## Notes For Future Revisions

For this shipped release, treat Atopile as the canonical electrical design and
the KiCad PCB as the canonical physical layout. Rebuilding from Atopile may
regenerate the PCB and remove hand-applied layout work, so do not overwrite the
layout unless you are intentionally starting a new board revision.
