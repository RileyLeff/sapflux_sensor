#!/usr/bin/env python3
"""Move all F.SilkS references to F.Fab in passive footprints (R, C, L).

Run after `ato build` to declutter the silkscreen layer.
Passives get their reference labels and outline graphics moved to F.Fab
so they can be hidden in KiCad without affecting IC/connector silk.
"""

import re
import sys
from pathlib import Path

PCB_PATH = Path(__file__).parent / "layouts" / "default" / "default.kicad_pcb"
PASSIVE_PREFIXES = {"R", "C", "L", "D"}


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

        # Count and replace F.SilkS → F.Fab within this footprint block
        count = block.count('"F.SilkS"')
        if count > 0:
            parts[i] = block.replace('"F.SilkS"', '"F.Fab"')
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
    print(f"Moved {n} F.SilkS → F.Fab references in passive footprints")
