#!/usr/bin/env python3
"""Place 48 RC input protection pairs and 12 TVS arrays aligned with CN2.

Reads the KiCad PCB, finds components by net, and moves them into
organized columns next to the 60-pin thermistor connector (CN2).

Layout (looking at board from top, CN2 on right edge):

    TVS | C_even | C_odd | R_even | R_odd | [connector]
    X=181  X=184    X=187   X=190    X=193    X=195-198

Each row corresponds to a pair of thermistors sharing the same Y position
on the connector. 0603 components at 0° rotation (horizontal).
TVS arrays (SOT-23-6) centered among their 4 thermistors.
"""

import math
import re
import sys
from pathlib import Path

PCB_PATH = Path(__file__).parent / "layouts" / "default" / "default.kicad_pcb"

# CN2 position and rotation from PCB
CN2_X, CN2_Y, CN2_ROT = 196.73, 89.95, -90.0
CN2_RAD = math.radians(CN2_ROT)

# X positions for protection columns (board coordinates)
R_ODD_X = 193.0   # Row 1 (odd pin) resistors
R_EVEN_X = 190.0   # Row 2 (even pin) resistors
C_ODD_X = 187.0    # Row 1 caps
C_EVEN_X = 184.0   # Row 2 caps
TVS_X = 181.0       # TVS arrays

# Component rotation (0° = horizontal, pads left-right)
RC_ROTATION = 0
TVS_ROTATION = 90  # SOT-23-6 rotated to fit vertically


def cn2_local_to_board(lx, ly):
    """Transform CN2 local pad coordinates to board coordinates."""
    bx = CN2_X + lx * math.cos(CN2_RAD) - ly * math.sin(CN2_RAD)
    by = CN2_Y + lx * math.sin(CN2_RAD) + ly * math.cos(CN2_RAD)
    return round(bx, 2), round(by, 2)


# All thermistor pads: (pad_num, local_x, local_y, therm_number)
THERM_PADS = [
    (3, -34.29, 1.27, 1), (4, -34.29, -1.27, 2),
    (5, -31.75, 1.27, 3), (6, -31.75, -1.27, 4),
    (7, -29.21, 1.27, 5), (8, -29.21, -1.27, 6),
    (9, -26.67, 1.27, 7), (10, -26.67, -1.27, 8),
    (13, -21.59, 1.27, 9), (14, -21.59, -1.27, 10),
    (15, -19.05, 1.27, 11), (16, -19.05, -1.27, 12),
    (17, -16.51, 1.27, 13), (18, -16.51, -1.27, 14),
    (19, -13.97, 1.27, 15), (20, -13.97, -1.27, 16),
    (23, -8.89, 1.27, 17), (24, -8.89, -1.27, 18),
    (25, -6.35, 1.27, 19), (26, -6.35, -1.27, 20),
    (27, -3.81, 1.27, 21), (28, -3.81, -1.27, 22),
    (29, -1.27, 1.27, 23), (30, -1.27, -1.27, 24),
    (33, 3.81, 1.27, 25), (34, 3.81, -1.27, 26),
    (35, 6.35, 1.27, 27), (36, 6.35, -1.27, 28),
    (37, 8.89, 1.27, 29), (38, 8.89, -1.27, 30),
    (39, 11.43, 1.27, 31), (40, 11.43, -1.27, 32),
    (43, 16.51, 1.27, 33), (44, 16.51, -1.27, 34),
    (45, 19.05, 1.27, 35), (46, 19.05, -1.27, 36),
    (47, 21.59, 1.27, 37), (48, 21.59, -1.27, 38),
    (49, 24.13, 1.27, 39), (50, 24.13, -1.27, 40),
    (53, 29.21, 1.27, 41), (54, 29.21, -1.27, 42),
    (55, 31.75, 1.27, 43), (56, 31.75, -1.27, 44),
    (57, 34.29, 1.27, 45), (58, 34.29, -1.27, 46),
    (59, 36.83, 1.27, 47), (60, 36.83, -1.27, 48),
]

# TVS groupings: each TVS covers 4 thermistors
TVS_GROUPS = [
    (1, 2, 3, 4), (5, 6, 7, 8), (9, 10, 11, 12), (13, 14, 15, 16),
    (17, 18, 19, 20), (21, 22, 23, 24), (25, 26, 27, 28), (29, 30, 31, 32),
    (33, 34, 35, 36), (37, 38, 39, 40), (41, 42, 43, 44), (45, 46, 47, 48),
]


def build_therm_positions():
    """Build dict: therm_number -> (board_x, board_y, is_odd_pin)."""
    positions = {}
    for pad_num, lx, ly, therm_n in THERM_PADS:
        bx, by = cn2_local_to_board(lx, ly)
        is_odd = (pad_num % 2 == 1)  # Row 1 = odd pin numbers
        positions[therm_n] = (bx, by, is_odd)
    return positions


def find_component_by_nets(footprints, net1, net2=None, exclude_refs=None):
    """Find a footprint reference by its pad nets."""
    exclude = exclude_refs or set()
    for block in footprints:
        ref_m = re.search(r'\(property\s+"Reference"\s+"([^"]+)"', block)
        if not ref_m:
            continue
        ref = ref_m.group(1)
        if ref in exclude:
            continue
        nets = set(re.findall(r'\(net\s+\d+\s+"([^"]+)"\)', block))
        if net2:
            if nets == {net1, net2}:
                return ref
        else:
            if net1 in nets:
                return ref
    return None


def build_net_to_ref_map(footprints):
    """Build maps: therm_N -> (R_ref, C_ref) and TVS group -> D_ref."""
    r_map = {}  # therm_N -> ref of 1k resistor
    c_map = {}  # therm_N -> ref of 100nF cap
    used = set()

    for block in footprints:
        ref_m = re.search(r'\(property\s+"Reference"\s+"([^"]+)"', block)
        if not ref_m:
            continue
        ref = ref_m.group(1)
        nets = set(re.findall(r'\(net\s+\d+\s+"([^"]+)"\)', block))

        # Resistors: connect therm_N to prot_N
        if ref.startswith("R"):
            for n in range(1, 49):
                if nets == {f"therm_{n}", f"prot_{n}"}:
                    r_map[n] = ref
                    used.add(ref)
                    break

        # Caps: connect prot_N to GND
        if ref.startswith("C"):
            for n in range(1, 49):
                if nets == {f"prot_{n}", "GND"}:
                    c_map[n] = ref
                    used.add(ref)
                    break

    # TVS arrays: connect to 4 therm_N nets + GND + V3V3_TVS
    tvs_map = {}  # group_index -> ref
    for block in footprints:
        ref_m = re.search(r'\(property\s+"Reference"\s+"([^"]+)"', block)
        if not ref_m:
            continue
        ref = ref_m.group(1)
        if not ref.startswith("D") or ref in used:
            continue
        nets = set(re.findall(r'\(net\s+\d+\s+"([^"]+)"\)', block))

        for gi, group in enumerate(TVS_GROUPS):
            expected = {f"therm_{n}" for n in group} | {"GND", "V3V3_TVS"}
            if nets == expected:
                tvs_map[gi] = ref
                break

    return r_map, c_map, tvs_map


def move_footprint(content, ref, new_x, new_y, new_rot=None):
    """Move a footprint to a new position in the PCB content string."""
    # Find the footprint block containing this reference
    pattern = re.compile(
        r'(\t\(footprint\s+"[^"]+"\s+)'           # footprint header
        r'(\(layer\s+"[^"]+"\)\s+)'                 # layer
        r'\(at\s+[\d.-]+\s+[\d.-]+(?:\s+[\d.-]+)?\)',  # current position
        re.MULTILINE
    )

    # We need to find the specific footprint with this reference
    # Split by footprints and process
    parts = re.split(r'(?=\t\(footprint )', content)

    for i, block in enumerate(parts):
        ref_m = re.search(r'\(property\s+"Reference"\s+"' + re.escape(ref) + r'"', block)
        if not ref_m:
            continue

        # Replace the (at ...) in this block
        if new_rot is not None:
            new_at = f"(at {new_x} {new_y} {new_rot})"
        else:
            new_at = f"(at {new_x} {new_y})"

        # Match the footprint-level (at ...) — it's the first one in the block
        at_pattern = re.compile(r'\(at\s+[\d.-]+\s+[\d.-]+(?:\s+[\d.-]+)?\)')
        match = at_pattern.search(block)
        if match:
            parts[i] = block[:match.start()] + new_at + block[match.end():]
            break

    return "".join(parts)


def main():
    pcb_path = Path(sys.argv[1]) if len(sys.argv) > 1 else PCB_PATH
    content = pcb_path.read_text()

    footprints = re.split(r'(?=\t\(footprint )', content)

    # Build position and component maps
    therm_pos = build_therm_positions()
    r_map, c_map, tvs_map = build_net_to_ref_map(footprints)

    print(f"Found {len(r_map)} resistors, {len(c_map)} caps, {len(tvs_map)} TVS arrays")

    # Verify completeness
    missing = []
    for n in range(1, 49):
        if n not in r_map:
            missing.append(f"R for therm_{n}")
        if n not in c_map:
            missing.append(f"C for therm_{n}")
    for gi in range(12):
        if gi not in tvs_map:
            missing.append(f"TVS for group {gi} ({TVS_GROUPS[gi]})")
    if missing:
        print(f"WARNING: Missing components: {missing}")

    moves = 0

    # Place resistors and caps
    for n in range(1, 49):
        bx, by, is_odd = therm_pos[n]

        if n in r_map:
            rx = R_ODD_X if is_odd else R_EVEN_X
            content = move_footprint(content, r_map[n], rx, by, RC_ROTATION)
            moves += 1

        if n in c_map:
            cx = C_ODD_X if is_odd else C_EVEN_X
            content = move_footprint(content, c_map[n], cx, by, RC_ROTATION)
            moves += 1

    # Place TVS arrays — centered Y among their 4 thermistors
    for gi, group in enumerate(TVS_GROUPS):
        if gi not in tvs_map:
            continue
        ys = [therm_pos[n][1] for n in group]
        center_y = sum(ys) / len(ys)
        content = move_footprint(content, tvs_map[gi], TVS_X, round(center_y, 2), TVS_ROTATION)
        moves += 1

    pcb_path.write_text(content)
    print(f"Moved {moves} components")


if __name__ == "__main__":
    main()
