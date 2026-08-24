# Field-enclosure mounts

Printed parts that put the sap-flux logic-board enclosure and the LiFePO4 pack
onto the perforated mounting plate of a QILIPSU 370 × 270 × 150 mm IP67 box.

Generated from `mounts.scad` (OpenSCAD). Re-export with:

```sh
openscad -D 'part="back_bracket"' -o back_bracket.stl mounts.scad
openscad -D 'part="peg_cap"'      -o peg_cap.stl      mounts.scad
openscad -D 'part="battery_tray"' -o battery_tray.stl mounts.scad
openscad -D 'part="assembly"'  --camera=0,-55,0,0,0,0,700 --projection=o \
         --imgsize=900,1150 -o asm_plate.png mounts.scad     # review artifact
```

## The mounting-plate grid

Measured: **3.0 mm square holes on a 5.0 mm pitch** in both directions (3 mm
hole, 2 mm web). An M3 passes a 3 mm square hole easily — the diagonal is
4.24 mm.

Every screw pattern here is an exact multiple of 5 mm **between holes**, so all
of a part's screws land on the grid simultaneously:

| Part | Pattern | Pitches |
|---|---|---|
| `back_bracket` | 115 × 50 mm | 23 × 10 |
| `battery_tray` | 50 mm columns, 20 mm rows | 10 × 4 |

The holes themselves are **slotted** (3.4 × 10 mm) so each part can be nudged
into phase with the grid during install. The tray's slots run *vertically* on
purpose: its height is fixed by the enclosure floor, so the slot has to absorb
whatever phase the grid happens to be in at that height.

**Both parts stand 4 mm off the plate face** — the bracket on four bosses, the
tray on two vertical rails — so screw heads or hardware already protruding from
the plate can't hold either part off.

## Parts

### `back_bracket` + 2 × `peg_cap`

136 × 66 × 3.5 mm plate (plus 4 mm bosses), ~33 cm³.

The logic-board enclosure's back face is flat and its two trapezoidal zip-tie
tabs are flush with it, so box back + tabs are coplanar and the whole thing lies
flat on the bracket. The Ø8 tie holes are **121.0 mm apart** (= 106 mm box
width + 2 × 7.5 mm tab offset), on the box centreline.

Each `peg_cap` is a Ø7.6 × 3.6 mm spigot under a Ø13 × 3 mm head. The spigot
drops through a tie hole and locates the box; the head lands on the **tab**
(the spigot is deliberately 0.4 mm shorter than the tab is thick) and clamps
it down. An M3 × 8 threads into an M3 nut captured in a hex pocket in the
bracket's back face — 2.1 mm of nut engaged, and nothing protrudes past the
bracket's back face into the plate.

To remove the box: back off the two M3 × 8, lift the caps, lift the box off.

The bracket also has four shallow Ø8.5 × 0.8 mm reliefs where the box's own
corner M3 nut pockets sit, in case a pressed nut sits a hair proud.

### `battery_tray`

102 × 89 × 80 mm, ~120 cm³ solid. Cups a **94.5 × 65.5 × 150.5 mm** pack
standing on end.

- Cup ID 96.0 × 67.0 mm (1.5 mm total slop), 3 mm walls, 30 mm lip.
- The pack's weight lands on the enclosure floor through the 3 mm tray floor —
  the back panel only has to stop it tipping.
- The back is a closed duct: outer skin at 4–9 mm from the plate face, a 7 mm
  cavity for protruding screw heads, and the cup's back wall at 16–19 mm
  carried full height. Two side webs close the section, so it's stiff against
  the pack leaning on it.
- Screwdriver access holes through the inner skin let you reach the six grid
  screws from inside the cup.
- Finger scallop in the front wall to lift the pack out; four Ø6 drain holes.

Battery top ends up **153.5 mm** above the enclosure floor; total depth from the
plate face is **89 mm** (interior depth is ~130 mm).

## Hardware

| Qty | Item | For |
|---|---|---|
| 4 | M3 × (12 + plate thickness) machine screw + nut + washer | bracket → plate |
| 6 | M3 × (12 + plate thickness) machine screw + nut + washer | tray → plate |
| 2 | **M3 × 8** pan head | peg caps (do not substitute longer) |
| 2 | M3 hex nut (DIN934, 5.5 AF) | press into the bracket's back-face pockets |

M3 × 16 is the likely length for the grid screws — confirm once the mounting
plate thickness is measured.

`M3 × 8` for the caps is not a guess: at 10 mm the tip protrudes 1.5 mm past the
bracket's back face and holds it off the plate.

## Printing

PETG. Three walls, 0.2 mm layers. **No supports needed on any part.**

| Part | Orientation |
|---|---|
| `back_bracket` | front face (the flat one) on the bed; bosses and nut pockets face up |
| `peg_cap` | head down on the bed, spigot up |
| `battery_tray` | floor on the bed; every wall is then vertical |

## Assembly

1. Press an M3 nut into each of the two hex pockets in the bracket's back face.
2. Bolt the bracket to the plate (4 × M3, nuts behind the plate). Slots give
   ±3.3 mm to find the grid.
3. Lay the box on the bracket, tabs over the two M3 holes. Drop a `peg_cap`
   into each tie hole, run in the M3 × 8, snug only.
4. Bolt the tray to the plate (6 × M3, driven from inside the cup through the
   access holes) with its floor resting on the enclosure floor.
5. Drop the pack in.

## Open

- **Mounting plate thickness** — sets the grid screw length.
- Access behind the plate for nuts. If there is none, M3 × 8 self-tappers driven
  straight into the 3 mm holes will hold, but won't survive many removals.
- Where the plate's own mounting screws sit, so the bracket and tray can be
  placed clear of them (the 4 mm standoff covers heads up to that height).
- Battery terminal face and wire routing — the cup doesn't care, but it decides
  which way round the pack goes in.
