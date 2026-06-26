#!/usr/bin/env python3
"""Move passive reference labels from F.SilkS to F.Fab.

Run after `ato build` to declutter the silkscreen layer.
Only the `Reference` property layer is changed for R, C, and L footprints.
Other silkscreen graphics, pin-1 dots, logos, and outlines are left alone.
"""

import re
import sys
from pathlib import Path

PCB_PATH = Path(__file__).parent / "layouts" / "default" / "default.kicad_pcb"
PASSIVE_PREFIXES = {"R", "C", "L"}


def _find_matching_paren(text: str, start: int) -> int:
    depth = 0
    in_string = False
    escaped = False

    for i in range(start, len(text)):
        ch = text[i]

        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            continue

        if ch == '"':
            in_string = True
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return i

    return -1


def _move_reference_property_layer(block: str) -> tuple[str, int]:
    ref_start = block.find('(property "Reference"')
    if ref_start < 0:
        return block, 0

    ref_end = _find_matching_paren(block, ref_start)
    if ref_end < 0:
        return block, 0

    ref_prop = block[ref_start : ref_end + 1]
    updated_prop = ref_prop.replace('(layer "F.SilkS")', '(layer "F.Fab")', 1)
    if updated_prop == ref_prop:
        return block, 0

    return block[:ref_start] + updated_prop + block[ref_end + 1 :], 1


def fix_passive_layers(pcb_path: Path) -> int:
    content = pcb_path.read_text()

    # Split into footprint blocks (preserve everything before first footprint)
    parts = re.split(r"(?=\t\(footprint )", content)
    total_fixes = 0

    for i, block in enumerate(parts):
        if not block.strip().startswith("(footprint"):
            continue

        # Check if this is a passive footprint
        ref_m = re.search(r'\(property\s+"Reference"\s+"([^"]+)"', block)
        if not ref_m:
            continue
        ref = ref_m.group(1)
        prefix = re.match(r"[A-Z]+", ref)
        if not prefix or prefix.group() not in PASSIVE_PREFIXES:
            continue

        parts[i], count = _move_reference_property_layer(block)
        total_fixes += count

    if total_fixes > 0:
        pcb_path.write_text("".join(parts))

    return total_fixes


if __name__ == "__main__":
    pcb = Path(sys.argv[1]) if len(sys.argv) > 1 else PCB_PATH
    if not pcb.exists():
        print(f"PCB file not found: {pcb}", file=sys.stderr)
        sys.exit(1)

    n = fix_passive_layers(pcb)
    print(f"Moved {n} passive reference labels from F.SilkS to F.Fab")
