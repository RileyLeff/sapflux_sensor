# Sap Flux Adapter Electronics

This directory contains the Atopile source for the sap flux adapter board.
Atopile is the canonical electrical design for the shipped prototype.

## Entry Point

- `main.ato` defines the top-level `SapFluxAdapter` module.
- `ato.yaml` defines the `default` build.
- `layouts/default/default.kicad_pcb` is the hand-routed KiCad layout that was
  fabricated by JLCPCB.

## Build

```bash
ato build
```

Run from this directory.

Be careful with rebuilds: the KiCad PCB includes hand placement, routing,
board-art, and late manufacturing fixes. For the board that was physically
fabricated, use the frozen release package in
`../releases/sap-flux-adapter-v1.1-shipped-2026-03-21/`.

## Tooling

This design was built with Atopile and KiCad. `pcbparts` was used heavily for
part discovery and package/footprint work while learning hardware design.
