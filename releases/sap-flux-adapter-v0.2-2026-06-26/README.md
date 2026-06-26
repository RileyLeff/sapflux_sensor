# Sap Flux Adapter v0.2 Manufacturing Release

Release date: 2026-06-26

Packaged from git commit `6a719cb2e322` on branch `codex/v0.2-board-fixes`.
This package was generated from the saved KiCad board file only; Atopile was not
rebuilt during packaging.

## JLCPCB submission files

Use the files in `jlcpcb/` for the order:

- `gerbers.zip` - PCB fabrication Gerbers + drill.
- `jlcpcb-bom.csv` - JLC assembly BOM.
- `jlcpcb-cpl.csv` - JLC assembly placement file.

`jlcpcb/default-pos.raw.csv` is retained only as the raw KiCad position export
used to make the JLC CPL.

## Source snapshot

The `source/` directory contains the KiCad board/project files and Atopile BOM
snapshot used to generate this release:

- `default.kicad_pcb`
- `default.kicad_pro`
- `default.kicad_dru`
- `fp-lib-table`
- `default.atopile-bom.csv`

## Validation

KiCad CLI: `10.0.3`

DRC report: `reports/kicad-drc-report.txt`

DRC summary:

- 414 DRC warnings.
- 0 unconnected pads.
- 0 footprint errors.
- Violation categories are limited to silkscreen/text warnings:
  - `silk_overlap`: 199
  - `silk_over_copper`: 199
  - `text_thickness`: 8
  - `text_height`: 8

BOM/CPL sanity:

- 233 populated references in BOM.
- 233 populated references in CPL.
- No BOM references missing from CPL.
- No CPL references missing from BOM.

Critical v0.2 part checks passed:

- `D2`: `SMAJ33CA`, LCSC `C19077549`
- `D20`: `BAT54C`, LCSC `C22466350`
- `F1`: `XF-508P-B-B`, LCSC `C19727305`

JLC BOM substitutions:

- `R22`: source part `0603WAF1102T5E` / `C25950` replaced in the JLC BOM by
  `RC0603FR-0711KL` / `C137801` due to JLC stock shortfall. Same package,
  resistance, tolerance, power, voltage rating, and temperature coefficient.
- `R84`, `R86`, `R88`: source part `0603WAF4704T5E` / `C23163` replaced in the
  JLC BOM by `FRC0603F4704TS` / `C2930153` due to JLC stock shortfall. Same
  package, resistance, tolerance, power, voltage rating, and temperature
  coefficient.

Obsolete v0.1/v1.1 parts intentionally absent from the JLC BOM:

- `BAT54S`
- `SMAJ33A`
- old `F1, F2` 5x20 fuse holder arrangement

## Order-form notes

Board outline is 100 mm x 65 mm. Confirm the intended JLC stackup/thickness in
the order form before purchase. KiCad's generated Gerber job metadata reports
`BoardThickness: 1.4204`, inherited from the project stackup model; use the JLC
order form as the source of truth for the physical board thickness.

Before paying for assembly, inspect orientation in JLC's viewer for all polarized
or pin-1-sensitive components, especially `D1-D21`, `U1-U13`, `Q1-Q8`, `BAT1`,
`CN1`, `J1`, `J3`, and `F1`.
