#!/usr/bin/env python3
"""Probe the lid in assembly.blend with a raycast grid to map OPEN vs SOLID.
    blender --background assembly.blend --python diag_lid.py
Prints an ASCII map over a region so we can see exactly what the q1 window did."""
import bpy, mathutils

Z_TOP = 19.62   # Z_LID_TOP from the generator

def probe(name, bx0, bx1, by0, by1, step=1.0):
    lid = bpy.data.objects.get("lid")
    deps = bpy.context.evaluated_depsgraph_get()
    print(f"\n== {name}: bx {bx0}..{bx1}, by {by0}..{by1}  ('#'=solid lid, '.'=open) ==")
    y = by1
    rows = []
    while y >= by0:
        row = ""
        x = bx0
        while x <= bx1:
            hit, *_ = lid.ray_cast(mathutils.Vector((x, y, Z_TOP + 2.0)),
                                   mathutils.Vector((0, 0, -1)), depsgraph=deps, distance=6.0)
            row += "#" if hit else "."
            x += step
        rows.append(f"  by={y:5.1f} |{row}|")
        y -= step
    print("\n".join(rows))
    print(f"  (bx {bx0}..{bx1} left->right, step {step}mm)")

# q1 window is bx 8.2..24.2, by 27.7..40.7 -> probe a bit wider
probe("Q1 region", 4.0, 28.0, 24.0, 44.0, step=1.0)
