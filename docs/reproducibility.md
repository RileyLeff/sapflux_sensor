# Reproducibility Notes

This project is an open hardware sap flux sensor adapter. The physical board
exists, but the measurement method still needs field validation. Tests are
planned for summer 2026 in the United States.

## Source Of Truth

Use the Atopile implementation as the canonical design source:

- `../elec/main.ato` - top-level wiring and board architecture.
- `../elec/ato.yaml` - Atopile build configuration.
- `../elec/*.ato` - subsystem modules.
- `../elec/parts/` - local part definitions, footprints, symbols, and models.

The planning documents in the repo are valuable history, but they may describe
earlier design intentions. When a planning document conflicts with Atopile,
Atopile wins.

## Shipped Layout

The shipped KiCad layout is:

- `../elec/layouts/default/default.kicad_pcb`

The layout is intentionally preserved under Atopile's generated `default` build
name because that is the file that accumulated the hand-routed placement,
routing, board art, and manufacturing fixes.

The versioned shipped artifact is:

- `../releases/sap-flux-adapter-v1.1-shipped-2026-03-21/`

That release directory contains the canonical March 21 Gerbers and JLCPCB
assembly files used after the late R19/BAT1 fix.

## Rebuilding

Atopile can regenerate the electrical project:

```bash
cd elec
ato build
```

Be careful: rebuilding can overwrite parts of the KiCad project and remove
manual layout work. The current hand-routed board should be treated as a shipped
artifact, not a disposable build cache.

## Tooling

Atopile made this project approachable by turning a hardware design into a
readable, modular source tree. `pcbparts` was also essential while learning:
it made part discovery, package checks, and KiCad/JLCPCB-oriented component work
much faster than manual datasheet-and-footprint hunting.

Those tools are part of the story of this board. They are not required to
interpret the frozen release package, but they are the right starting point for
future revisions.
