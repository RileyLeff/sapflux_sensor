#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["matplotlib"]
# ///
"""Top-down plan: lid access windows overlaid on real board component keep-outs.
Run: uv run enclosure/plan_view.py   ->  enclosure/plan_view.png
Coords are board-local mm: bx 0..100 (right), by 0..65 (toward 60-pin edge).
Figure is drawn as if looking down at the TOP face (by increases downward)."""
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import Rectangle, Circle
import os

BOARD_W, BOARD_H = 100.0, 65.0
WALL, CLR = 2.4, 0.6     # flush walls (I/O-shield style), no outward shroud
OUT = (-CLR-WALL, BOARD_W+CLR+WALL, -CLR-WALL, BOARD_H+CLR+WALL)
CORNERS = [(4,4),(96,4),(4,61),(96,61)]

# Measured component keep-outs (bx0,bx1,by0,by1, label)  -- from pad/footprint extents
KEEPOUTS = [
    (3.9,6.1, 9.1,14.8,  "CN1 power"),
    (3.9,6.1, 25.0,30.6, "J1 heater"),
    (3.8,6.2, 46.7,56.2, "J3 RS485"),
    (1.6,8.8, 16.8,23.4, "U12 7.5mm cap"),
    (13.0,41.0, 10.0,13.5,"5x20 FUSE"),
    (10.5,21.9, 31.2,37.2,"Q1 +HS"),
    (8.8,17.4, 1.5,6.8,   "U7 PTC"),
    (24.2,29.1, 1.5,5.0,  "L1"),
    (31.7,41.1, 1.2,2.8,  "J4 SWD"),
    (42.6,57.1, 1.1,2.9,  "J2 aux"),
    (38.5,45.5, 22.6,29.6,"MCU"),
    (87.0,96.0, 15.3,43.7,"CR2032 28mm"),
    (11.3,86.5, 59.3,63.4,"CN2 60-pin"),
]
TESTPADS = [(59.93,"3V3"),(65.93,"24"),(62.93,"G"),(68.93,"GA"),
            (71.93,"PG"),(74.93,"RST"),(77.93,"PNP"),(80.93,"EXC")]
# Lid access windows (kept in sync with generate_enclosure.py)
WINDOWS = [
    ("top_io", 29,83,-2,6),
    ("fuse",   12,41,7,22),      # extended up + rounded finger scoop
    ("cr2032", 79,99,11,47),     # widened inward, edge side (x1=99) kept
    ("ribbon", 10,88,56,67),
    ("q1",     8.2,24.2,27.7,40.7),  # smaller (PLACEHOLDER heatsink)
]
# Terminal lid-ports (screws face UP -> reached through these; wires exit the LEFT wall):
PORTS = [("power",-0.6,10,7.5,17),("heater",-0.6,10,22.5,33),("rs485",-0.6,10,44.5,57.5)]
LOGO = (54.0, 34.0, 20.0)       # cx, cy, ~radius (riley_logo RAISED relief on lid)

fig, ax = plt.subplots(figsize=(13, 9))
# enclosure outer + shroud
ax.add_patch(Rectangle((OUT[0],OUT[2]), OUT[1]-OUT[0], OUT[3]-OUT[2],
             fill=False, ec="0.4", lw=1.5, ls=(0,(6,4))))
# board
ax.add_patch(Rectangle((0,0), BOARD_W, BOARD_H, fill=False, ec="black", lw=2))

# component keep-outs
for x0,x1,y0,y1,lbl in KEEPOUTS:
    ax.add_patch(Rectangle((x0,y0), x1-x0, y1-y0, fc="#d9b38c", ec="#8a5a2b", lw=1, alpha=0.85))
    ax.text((x0+x1)/2,(y0+y1)/2, lbl, ha="center", va="center", fontsize=6.5, rotation=0)
for x,lbl in TESTPADS:
    ax.add_patch(Circle((x,2.1), 0.9, fc="#c0c0c0", ec="black", lw=0.5))

# lid windows (translucent green on top)
for name,x0,x1,y0,y1 in WINDOWS:
    ax.add_patch(Rectangle((x0,y0), x1-x0, y1-y0, fc="#3fa34d", ec="#1d5e27", lw=2, alpha=0.32))
    ax.text((x0+x1)/2, (y0+y1)/2, f"WINDOW\n{name}", ha="center", va="center",
            fontsize=8, color="#14401b", fontweight="bold")

# terminal lid-ports: screwdriver access from the top; wires leave via the left wall notch
for name,x0,x1,y0,y1 in PORTS:
    ax.add_patch(Rectangle((x0,y0), x1-x0, y1-y0, fc="#2f7fd6", ec="#143f6e", lw=2, alpha=0.40))
    ax.text((x0+x1)/2,(y0+y1)/2, f"PORT\n{name}\nscrews up\nwires<-wall", ha="center", va="center",
            fontsize=6, color="#0e2a4a", fontweight="bold")

# logo cutout region
lx,ly,lr = LOGO
ax.add_patch(Circle((lx,ly), lr, fill=False, ec="#2f9e44", lw=1.5, ls=(0,(4,3))))
ax.text(lx, ly, "riley logo\n(deferred)", ha="center", va="center", fontsize=7, color="#2f9e44", fontweight="bold")

# standoff posts / M3 corner screws
for cx,cy in CORNERS:
    ax.add_patch(Circle((cx,cy), 3.0, fc="#7a7aff", ec="#22228a", lw=1, alpha=0.8))
    ax.add_patch(Circle((cx,cy), 1.6, fc="white", ec="#22228a", lw=0.8))

ax.set_xlim(OUT[0]-2, OUT[1]+2)
ax.set_ylim(OUT[3]+2, OUT[2]-2)   # invert Y -> top edge at top of figure
ax.set_aspect("equal")
ax.set_title("Sap-flux enclosure LID plan: green=windows, blue=terminal ports (exit UP), "
             "dashed=logo\nTOP=test pads | LEFT=top-entry screw terminals | RIGHT=CR2032 | BOTTOM=60-pin",
             fontsize=10)
ax.set_xlabel("bx (mm)"); ax.set_ylabel("by (mm)")
ax.grid(True, ls=":", alpha=0.3)
here = os.path.dirname(os.path.abspath(__file__))
out = os.path.join(here, "plan_view.png")
fig.tight_layout(); fig.savefig(out, dpi=130)
print("wrote", out)
