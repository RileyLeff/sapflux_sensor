# Design History

This repository contains several planning and review documents written while
the board was evolving. They are useful for understanding the design process,
tradeoffs, and false starts.

For the shipped board, treat the Atopile source in `../../elec/` and the
versioned release package in `../../releases/` as canonical.

Suggested classification:

- `../../riley_design.md` - long-form design document; partially historical.
- `../../proposed_v1.1_revisions.md` - planning notes for the v1.1 direction.
- `../../plan_update.md` - patch-plan notes from review rounds.
- `../../POST_BUILD.md` - notes about manual post-build layout restoration.
- `../../cost_and_battery.md` - cost and battery comparison notes.

Known mismatch examples to resolve before treating old docs as current specs:

- Some docs describe `2x ADS1115` and six direct thermistor channels, while the
  shipped Atopile design uses `1x ADS1115`, three analog multiplexers, and 48
  thermistor channels.
- Some docs describe an M8 field connector, while the shipped prototype uses
  screw terminals for power and RS-485.
- Some docs describe `R_EXC = 560 ohm`, while the shipped Atopile source uses
  `270 ohm`.
