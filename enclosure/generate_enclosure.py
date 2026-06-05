#!/usr/bin/env python3
"""
Sap-flux adapter board enclosure generator (Blender headless / bpy).

Run:
    blender --background --python enclosure/generate_enclosure.py
Outputs (next to this script):
    base.stl        - backplate + FLUSH walls + standoff posts (no wall cutouts)
    lid.stl         - drop-on cover (Pi-case style): access windows + terminal ports + raised logo relief + vents
    assembly.blend  - base + lid + REAL board (from KiCad glb), all in one scene
    preview.png     - exploded review render

All dimensions in mm. Geometry is built numerically in mm and STL is exported
with global_scale=1.0, so slicers read it as mm.

Board-local frame: origin = KiCad (100, 85);  bx = X_kicad-100 (0..100),
by = Y_kicad-85 (0..65), z = 0 at backplate bottom, +z up.
Edges: by~0 TOP=test pads/SWD/aux | by~65 BOTTOM=60-pin | bx~0 LEFT=screw
terminals | bx~100 RIGHT=CR2032.

TERMINAL DIRECTION (verified from datasheets + photos + glb, 2026-06-05):
  Board headers are KF2EDGV-3.5 (C441321 2P / C441322 3P) = VERTICAL sockets
  open at the TOP (9.2mm tall). The mating plugs are KF2EDGK-3.5 (C440847 2P /
  C440848 3P): they drop straight DOWN onto the socket. On the plug, the SCREWS
  face UP and the WIRE-ENTRY holes are on a perpendicular face -> the wires exit
  HORIZONTALLY to the side. So each terminal needs BOTH: a LID PORT above it
  (screwdriver access + the plug body pokes through) AND a LEFT-WALL NOTCH beside
  it (the wires leave the box horizontally). Walls are flush (no outward shroud).
"""

import bpy, bmesh, math, os
import mathutils

# ======================================================================
# PARAMETERS  (tweak these)
# ======================================================================
BOARD_W      = 100.0
BOARD_H      = 65.0
BOARD_T      = 1.6        # MEASURE your fab board (KiCad stackup said 1.42)

CLR          = 0.6        # board edge -> inner wall gap
WALL         = 2.4
FLOOR        = 2.5
LID          = 2.5
STANDOFF_H   = 4.0        # board bottom above floor (clears ~1.5mm clipped back leads)

POST_OD      = 6.0
PILOT_D      = 2.5        # base post pilot for M3 self-tap
SCREW_CLR_D  = 3.4
HEAD_CB_D    = 6.5
HEAD_CB_DEPTH= 2.2

# --- box height -------------------------------------------------------
# Terminals/fuse/headers/60-pin/CR2032 all poke UP through lid windows, so they
# do NOT set the lid height.  The tallest component TRAPPED under solid lid is
# U12 (buck cap, 7.52mm, measured from the glb).  Clear it + margin.
TRAPPED_MAX   = 7.52      # U12 polymer cap, bx1.6-8.8 by16.8-23.4 (measured)
TRAPPED_CLR   = 1.5
UNDER_LID_GAP = TRAPPED_MAX + TRAPPED_CLR     # = 9.02mm

# Mated terminal stack (FYI, drives how far plugs poke ABOVE the lid):
HDR_H_ABOVE_BOARD = 9.2   # KF2EDGV-3.5 socket height above PCB (datasheet)
PLUG_BODY_H       = 11.1  # KF2EDGK-3.5 plug body height (datasheet) -> top ~20mm above PCB + wires

ENABLE_VENTS = True
VENT_D       = 4.0
VENT_PITCH   = 9.0

# --- Q1 heatsink bounding box (PLACEHOLDER - set to your real heatsink!) ---
# Shrunk per request.  Q1 (DPAK) footprint is bx10.5-21.9, by31.2-37.2.
HS_CENTER_BX = 16.2
HS_CENTER_BY = 34.2
HS_L         = 13.0       # along bx  (was 20 - shrunk)
HS_W         = 10.0       # along by  (was 20 - shrunk)
HS_WIN_MARGIN= 1.5

INCLUDE_BOARD = True
BOARD_GLB     = "board.glb"   # produced by: kicad-cli pcb export glb ...

# --- logo (RAISED RELIEF, not a cutout) -------------------------------
# Negative-space cutout was a printability nightmare: the eye dots/pupils, hair
# chunks and letter counters are islands with nothing holding them to the lid, so
# they'd fall out.  Instead extrude the art UP as a raised relief sitting ON the
# solid lid (rooted LOGO_EMBED into it) and MESH-JOIN it -> every island is just a
# supported bump, prints clean, and no fragile boolean to hang Blender.
ENABLE_LOGO  = False     # TODO: re-enable once the SVG->relief geometry is reworked
                         # (current fill+offset pipeline explodes into garbage islands).
LOGO_SVG     = "riley_logo.svg"
LOGO_W       = 40.0       # target width (mm) of the logo art
LOGO_CX      = 54.0       # centre in board-local bx
LOGO_CY      = 34.0       # centre in board-local by
LOGO_CLEAR_R = 25.0       # suppress vents within this radius of the logo centre
LOGO_DILATE  = 1.1        # fatten hand-drawn strokes (mm) so the relief is robust to print
LOGO_RELIEF_H= 1.2        # how far the logo stands proud of the lid top (mm)
LOGO_EMBED   = 0.6        # how far it roots into the lid for a clean weld (mm)

CORNERS = [(4.0, 4.0), (96.0, 4.0), (4.0, 61.0), (96.0, 61.0)]

# ======================================================================
# Derived Z levels and extents
# ======================================================================
Z_FLOOR_TOP = FLOOR
Z_BOARD_BOT = FLOOR + STANDOFF_H
Z_BOARD_TOP = Z_BOARD_BOT + BOARD_T
Z_LID_UNDER = Z_BOARD_TOP + UNDER_LID_GAP
Z_LID_TOP   = Z_LID_UNDER + LID

IN_X0 = -CLR
IN_X1 = BOARD_W + CLR
IN_Y0 = -CLR
IN_Y1 = BOARD_H + CLR
OUT_X0 = IN_X0 - WALL
OUT_X1 = IN_X1 + WALL
OUT_Y0 = IN_Y0 - WALL
OUT_Y1 = IN_Y1 + WALL

HS_X0 = HS_CENTER_BX - HS_L/2 - HS_WIN_MARGIN
HS_X1 = HS_CENTER_BX + HS_L/2 + HS_WIN_MARGIN
HS_Y0 = HS_CENTER_BY - HS_W/2 - HS_WIN_MARGIN
HS_Y1 = HS_CENTER_BY + HS_W/2 + HS_WIN_MARGIN

# Lid cuts: dict(name, x0,x1,y0,y1, r)  -- r = corner radius (0 = sharp)
# Access windows (things you look at / reach through the top):
WINDOWS = [
    dict(name="top_io", x0=29.0, x1=83.0, y0=-2.0, y1=6.0,  r=0.0),  # SWD + aux + test pads
    dict(name="fuse",   x0=12.0, x1=41.0, y0=7.0,  y1=22.0, r=4.0),  # 5x20 fuse, finger scoop up into freed Q1 space
    dict(name="cr2032", x0=79.0, x1=99.0, y0=11.0, y1=47.0, r=6.0),  # coin: edge side (x1=99) kept, inner side scooped
    dict(name="ribbon", x0=10.0, x1=88.0, y0=56.0, y1=67.0, r=0.0),  # 60-pin + IDC ribbon
    dict(name="q1",     x0=HS_X0, x1=HS_X1, y0=HS_Y0, y1=HS_Y1, r=3.0),  # heatsink (smaller, PLACEHOLDER)
]
# Terminal I/O-shield ports (top-entry plugs pass straight UP through these):
TERMINAL_PORTS = [
    dict(name="t_power",  x0=IN_X0, x1=10.0, y0=7.5,  y1=17.0, r=2.0),  # CN1 2P  (by7.9-16.0)
    dict(name="t_heater", x0=IN_X0, x1=10.0, y0=22.5, y1=33.0, r=2.0),  # J1  2P  (by23.8-31.8)
    dict(name="t_rs485",  x0=IN_X0, x1=10.0, y0=44.5, y1=57.5, r=2.0),  # J3  3P  (by45.8-57.1)
]
LID_CUTS = WINDOWS + TERMINAL_PORTS

# ======================================================================
# Geometry helpers
# ======================================================================
def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)

def add_box(name, x0, x1, y0, y1, z0, z1):
    me = bpy.data.meshes.new(name)
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    bm = bmesh.new()
    v = [bm.verts.new(p) for p in [
        (x0,y0,z0),(x1,y0,z0),(x1,y1,z0),(x0,y1,z0),
        (x0,y0,z1),(x1,y0,z1),(x1,y1,z1),(x0,y1,z1)]]
    for f in [(0,1,2,3),(7,6,5,4),(0,4,5,1),(1,5,6,2),(2,6,7,3),(3,7,4,0)]:
        bm.faces.new([v[i] for i in f])
    bmesh.ops.recalc_face_normals(bm, faces=bm.faces[:])  # REQUIRED or boolean no-ops
    bm.normal_update()
    bm.to_mesh(me); bm.free()
    return ob

def add_cyl(name, x, y, z0, z1, d, verts=64):
    bpy.ops.mesh.primitive_cylinder_add(vertices=verts, radius=d/2.0,
                                        depth=(z1-z0), location=(x, y, (z0+z1)/2.0))
    ob = bpy.context.active_object
    ob.name = name
    return ob

def boolean(obj, cutter, op='DIFFERENCE'):
    m = obj.modifiers.new(type='BOOLEAN', name='b')
    m.operation = op; m.object = cutter; m.solver = 'EXACT'
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.modifier_apply(modifier=m.name)
    bpy.data.objects.remove(cutter, do_unlink=True)

def join_objs(objs):
    for o in bpy.context.selected_objects:
        o.select_set(False)
    for o in objs:
        o.select_set(True)
    bpy.context.view_layer.objects.active = objs[0]
    bpy.ops.object.join()
    return objs[0]

def rrect_parts(name, x0, x1, y0, y1, z0, z1, r):
    """Axis-aligned rounded-rectangle prism as a LIST of CONVEX sub-solids (2 boxes +
    4 corner cylinders), deliberately NOT joined.  The caller subtracts each part on its
    own.  Joining the two overlapping boxes into one cutter makes a self-intersecting
    mesh that the EXACT solver subtracts only PARTIALLY -- that is exactly what left the
    q1 window's centre un-cut (only its 4 corner cylinders came through).  Each convex
    part subtracted individually is bulletproof."""
    r = max(0.0, min(r, (x1-x0)/2 - 0.01, (y1-y0)/2 - 0.01))
    if r <= 0.0:
        return [add_box(name, x0, x1, y0, y1, z0, z1)]
    parts = [
        add_box(name+"_a", x0+r, x1-r, y0,   y1,   z0, z1),
        add_box(name+"_b", x0,   x1,   y0+r, y1-r, z0, z1),
    ]
    for j, (cx, cy) in enumerate([(x0+r,y0+r),(x1-r,y0+r),(x0+r,y1-r),(x1-r,y1-r)]):
        parts.append(add_cyl(f"{name}_c{j}", cx, cy, z0, z1, 2*r, verts=32))
    return parts

def export_stl(obj, path):
    for o in bpy.context.selected_objects:
        o.select_set(False)
    obj.select_set(True)
    bpy.context.view_layer.objects.active = obj
    bpy.ops.wm.stl_export(filepath=path, export_selected_objects=True,
                          global_scale=1.0, apply_modifiers=True)

# ======================================================================
def build_base():
    """Backplate + FLUSH walls + integral standoff posts.  The left wall is notched
    at each terminal so the screw-plug WIRES (which exit horizontally to the side) can
    leave the box; the SCREWS (which face up) are reached through the matching lid port.
    Together each terminal sits in an open top+outward corner."""
    base = add_box("base", OUT_X0, OUT_X1, OUT_Y0, OUT_Y1, 0.0, Z_LID_UNDER)
    cavity = add_box("cavity", IN_X0, IN_X1, IN_Y0, IN_Y1, Z_FLOOR_TOP, Z_LID_UNDER + 5.0)
    boolean(base, cavity, 'DIFFERENCE')
    for i, (cx, cy) in enumerate(CORNERS):
        post = add_cyl(f"post{i}", cx, cy, Z_FLOOR_TOP - 0.6, Z_BOARD_BOT, POST_OD)
        boolean(base, post, 'UNION')
    cutters = []
    for i, (cx, cy) in enumerate(CORNERS):
        cutters.append(add_cyl(f"pilot{i}", cx, cy, Z_BOARD_BOT - 5.0, Z_BOARD_BOT + 0.1, PILOT_D))
    # left-wall wire-exit notches, directly below each lid port (open corner)
    for c in TERMINAL_PORTS:
        cutters.append(add_box("wn_"+c["name"], OUT_X0 - 1.0, IN_X0 + 1.0, c["y0"], c["y1"],
                               Z_BOARD_TOP - 1.0, Z_LID_UNDER + 0.5))
    cut = join_objs(cutters)
    boolean(base, cut, 'DIFFERENCE')
    base.name = "base"
    return base

def _in_any_cut(x, y, m=3.0):
    for c in LID_CUTS:
        if (c["x0"]-m) <= x <= (c["x1"]+m) and (c["y0"]-m) <= y <= (c["y1"]+m):
            return True
    return False

def _near_post(x, y, m=POST_OD/2 + 3.0):
    return any(math.hypot(x - cx, y - cy) < m for cx, cy in CORNERS)

def _near_logo(x, y):
    return ENABLE_LOGO and math.hypot(x - LOGO_CX, y - LOGO_CY) < LOGO_CLEAR_R

def build_logo_relief():
    """Import the SVG line-art, drop the white background rect, scale/centre it over the
    lid, and extrude it UPWARD as a RAISED relief that sits ON the solid lid top -- rooted
    LOGO_EMBED mm into the lid so it welds, standing LOGO_RELIEF_H proud above it.  Returns
    a single mesh object (to be MESH-JOINED, not booleaned, onto the lid) or None.

    Why relief, not a cutout: as negative space the eye dots/pupils, hair chunks and the
    insides of letters are islands with no material connecting them to the lid -> they'd
    drop out and can't print.  As a raised relief each island is just a bump supported by
    the solid lid underneath.  And because we mesh-join (the slicer unions overlapping
    positive volumes) there is NO EXACT boolean on the messy hand-drawn mesh -> can't hang."""
    here = os.path.dirname(os.path.abspath(__file__)) if "__file__" in globals() else os.getcwd()
    path = os.path.join(here, LOGO_SVG)
    if not (ENABLE_LOGO and os.path.exists(path)):
        print("logo svg not found/disabled - skipping logo")
        return None
    before = set(bpy.data.objects)
    bpy.ops.import_curve.svg(filepath=path)
    new = [o for o in bpy.data.objects if o not in before]
    curves = [o for o in new if o.type == 'CURVE']
    if not curves:
        print("logo: no curves imported"); return None
    def bbox_xy(o):
        pts = [o.matrix_world @ mathutils.Vector(c) for c in o.bound_box]
        xs = [p.x for p in pts]; ys = [p.y for p in pts]
        return (max(xs)-min(xs)), (max(ys)-min(ys))
    # drop the white background rect = the curve with the largest XY bbox area
    curves.sort(key=lambda o: (lambda w,h: w*h)(*bbox_xy(o)), reverse=True)
    bpy.data.objects.remove(curves[0], do_unlink=True)
    art = curves[1:]
    if not art:
        print("logo: nothing left after dropping bg"); return None
    # bake the SVG importer's object-scale into the curve data so offset (data units)
    # and the later mesh scale are in one consistent unit -> predictable dilation.
    for o in bpy.context.selected_objects: o.select_set(False)
    for c in art: c.select_set(True)
    bpy.context.view_layer.objects.active = art[0]
    bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
    # raw art width -> scale factor; dilate strokes so final width >= LOGO_DILATE mm
    axs = []
    for o in art:
        pts = [o.matrix_world @ mathutils.Vector(c) for c in o.bound_box]
        axs += [p.x for p in pts]
    raw_w = (max(axs) - min(axs)) or 1.0
    s = LOGO_W / raw_w
    for c in art:
        c.data.dimensions = '2D'
        c.data.fill_mode = 'BOTH'
        c.data.offset = LOGO_DILATE / (2.0 * s)   # fatten filled strokes (per-side)
    for o in bpy.context.selected_objects: o.select_set(False)
    for c in art: c.select_set(True)
    bpy.context.view_layer.objects.active = art[0]
    bpy.ops.object.convert(target='MESH')
    meshes = [o for o in art if o.type == 'MESH']
    logo = join_objs(meshes)
    logo.name = "logo_relief"
    bpy.context.view_layer.objects.active = logo
    bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)
    logo.scale = (s, s, 1.0)
    bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
    # centre in XY over the lid; set Z so a symmetric solidify spans
    # [Z_LID_TOP-EMBED .. Z_LID_TOP+RELIEF_H] -> rooted in the lid, standing proud above.
    pts = [logo.matrix_world @ mathutils.Vector(c) for c in logo.bound_box]
    xs = [p.x for p in pts]; ys = [p.y for p in pts]; zs = [p.z for p in pts]
    cx = (max(xs)+min(xs))/2; cy = (max(ys)+min(ys))/2; cz = (max(zs)+min(zs))/2
    z_mid = Z_LID_TOP + (LOGO_RELIEF_H - LOGO_EMBED) / 2.0
    logo.location = (LOGO_CX - cx, LOGO_CY - cy, z_mid - cz)
    bpy.ops.object.transform_apply(location=True, rotation=False, scale=False)
    # clean the flat filled mesh (offset can leave doubles / flipped faces)
    bpy.context.view_layer.objects.active = logo
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_all(action='SELECT')
    bpy.ops.mesh.remove_doubles(threshold=0.02)
    bpy.ops.mesh.normals_make_consistent(inside=False)
    bpy.ops.object.mode_set(mode='OBJECT')
    # extrude into a solid raised relief (symmetric about z_mid) via solidify
    sol = logo.modifiers.new(type='SOLIDIFY', name='sld')
    sol.thickness = LOGO_EMBED + LOGO_RELIEF_H
    sol.offset = 0.0
    bpy.ops.object.modifier_apply(modifier=sol.name)
    rz = [(logo.matrix_world @ mathutils.Vector(c)).z for c in logo.bound_box]
    print(f"logo relief: {len(meshes)} paths, x{s:.3f} -> {LOGO_W:.0f}mm at "
          f"({LOGO_CX},{LOGO_CY}), z {min(rz):.2f}..{max(rz):.2f} "
          f"(lid top {Z_LID_TOP:.2f}, +{LOGO_RELIEF_H:.1f} proud, -{LOGO_EMBED:.1f} rooted)")
    return logo

def build_lid():
    lid = add_box("lid", OUT_X0, OUT_X1, OUT_Y0, OUT_Y1, Z_LID_UNDER, Z_LID_TOP)  # drop-on cap
    for i, (cx, cy) in enumerate(CORNERS):
        pil = add_cyl(f"pillar{i}", cx, cy, Z_BOARD_TOP, Z_LID_TOP, POST_OD)
        boolean(lid, pil, 'UNION')
    z0 = Z_LID_UNDER - 1.0; z1 = Z_LID_TOP + 1.0
    # Windows + ports are subtracted as individual CONVEX parts: windows overlap each
    # other (e.g. q1 vs heater port) AND a rounded rect is itself two overlapping boxes,
    # so any joined cutter is self-intersecting -> EXACT subtracts it only partially.
    # Subtracting each convex sub-solid one at a time is robust to all of that.
    for c in LID_CUTS:
        for part in rrect_parts("win_"+c["name"], c["x0"], c["x1"], c["y0"], c["y1"], z0, z1, c["r"]):
            boolean(lid, part, 'DIFFERENCE')
    # corner screw clearance + concentric counterbore, per corner
    for i, (cx, cy) in enumerate(CORNERS):
        boolean(lid, add_cyl(f"sclr{i}", cx, cy, Z_BOARD_TOP - 1.0, z1, SCREW_CLR_D), 'DIFFERENCE')
        boolean(lid, add_cyl(f"scb{i}", cx, cy, Z_LID_TOP - HEAD_CB_DEPTH, z1, HEAD_CB_D), 'DIFFERENCE')
    # vents are uniformly spaced (no mutual overlap) -> safe to join into one cutter
    if ENABLE_VENTS:
        vents = []
        x = 6.0
        while x < BOARD_W:
            y = 6.0
            while y < BOARD_H:
                if not _in_any_cut(x, y) and not _near_post(x, y) and not _near_logo(x, y):
                    vents.append(add_cyl(f"v_{x:.0f}_{y:.0f}", x, y, z0, z1, VENT_D, verts=24))
                y += VENT_PITCH
            x += VENT_PITCH
        if vents:
            boolean(lid, join_objs(vents), 'DIFFERENCE')
    # Logo LAST, as a RAISED relief MESH-JOINED onto the lid (NO boolean): the relief is
    # rooted into the lid top so the slicer welds the overlap, and skipping the EXACT
    # solver on the messy hand-drawn mesh is what keeps Blender from hanging.
    relief = build_logo_relief()
    if relief is not None:
        lid = join_objs([lid, relief])
    lid.name = "lid"
    return lid

# ======================================================================
def verify_holes():
    lid = bpy.data.objects.get("lid")
    deps = bpy.context.evaluated_depsgraph_get()
    # (name, x, y, want_solid, z_start, dist)
    checks = [(c["name"], (c["x0"]+c["x1"])/2, (c["y0"]+c["y1"])/2, False, Z_LID_TOP + 0.5, 3.0)
              for c in LID_CUTS]
    checks.append(("solid_ref", 50.0, 12.0, True, Z_LID_TOP + 0.5, 3.0))   # known-solid, away from cuts
    # logo is now a RAISED relief on solid lid -> centre must read SOLID; ray starts above it
    checks.append(("logo_ctr", LOGO_CX, LOGO_CY, True, Z_LID_TOP + LOGO_RELIEF_H + 1.0,
                   LOGO_RELIEF_H + 4.0))
    print("\n-- lid cut verification (OPEN expected for windows/ports, SOLID for logo) --")
    for name, x, y, want_solid, z0, dist in checks:
        hit, *_ = lid.ray_cast(mathutils.Vector((x, y, z0)),
                               mathutils.Vector((0, 0, -1)), depsgraph=deps, distance=dist)
        ok = (hit == want_solid)
        print(f"   {name:<14} {'SOLID' if hit else 'OPEN '}  {'ok' if ok else '*** UNEXPECTED ***'}")

def import_board(path):
    if not (INCLUDE_BOARD and os.path.exists(path)):
        print("board glb not found/disabled - assembly will be enclosure-only")
        return None
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(filepath=path)
    new = [o for o in bpy.data.objects if o not in before]
    roots = [o for o in new if (o.parent is None or o.parent not in new)]
    xs = []
    for o in new:
        if o.type == 'MESH':
            for c in o.bound_box:
                xs.append((o.matrix_world @ mathutils.Vector(c)).x)
    raw_xspan = (max(xs) - min(xs)) if xs else 100.0
    factor = 1000.0 if raw_xspan < 10.0 else 1.0   # <10 => metres, scale to mm
    emp = bpy.data.objects.new("BOARD", None)
    bpy.context.collection.objects.link(emp)
    for r in roots:
        r.parent = emp
    emp.scale = (factor, -factor, factor)
    emp.location = (-100.0, -85.0, Z_BOARD_BOT)
    print(f"board imported: {len(new)} meshes, raw_xspan={raw_xspan:.3f} -> factor={factor:.0f}")
    return emp

# ======================================================================
def render(base, lid, path, explode=35.0):
    try:
        lid.location = (0, 0, explode)
        tgt = bpy.data.objects.new("tgt", None); bpy.context.collection.objects.link(tgt)
        tgt.location = (BOARD_W/2, BOARD_H/2, 12)
        cd = bpy.data.cameras.new("c"); cd.type = 'ORTHO'; cd.ortho_scale = 150
        cam = bpy.data.objects.new("c", cd); bpy.context.collection.objects.link(cam)
        cam.location = (BOARD_W/2 + 95, OUT_Y0 - 95, 110)
        con = cam.constraints.new('TRACK_TO'); con.target = tgt
        con.track_axis = 'TRACK_NEGATIVE_Z'; con.up_axis = 'UP_Y'
        sc = bpy.context.scene
        sc.camera = cam; sc.render.engine = 'BLENDER_WORKBENCH'
        sc.display.shading.show_cavity = True
        sc.render.resolution_x = 1500; sc.render.resolution_y = 1050
        sc.render.filepath = path
        bpy.ops.render.render(write_still=True)
        lid.location = (0, 0, 0)
        bpy.data.objects.remove(cam, do_unlink=True)
        bpy.data.objects.remove(tgt, do_unlink=True)
    except Exception as e:
        print("render skipped:", e)

# ======================================================================
def main():
    here = os.path.dirname(os.path.abspath(__file__)) if "__file__" in globals() else os.getcwd()
    reset()
    sc = bpy.context.scene
    sc.unit_settings.system = 'METRIC'
    sc.unit_settings.length_unit = 'MILLIMETERS'
    sc.unit_settings.scale_length = 0.001   # display BU as mm in the .blend

    base = build_base()
    lid = build_lid()
    verify_holes()

    export_stl(base, os.path.join(here, "base.stl"))
    export_stl(lid,  os.path.join(here, "lid.stl"))
    render(base, lid, os.path.join(here, "preview.png"))

    import_board(os.path.join(here, BOARD_GLB))
    for o in bpy.data.objects:
        o.hide_render = False
    bpy.ops.wm.save_as_mainfile(filepath=os.path.join(here, "assembly.blend"))

    print("\n================ ENCLOSURE SUMMARY ================")
    print(f"Style              : flush walls; terminals -> lid port (screws up) + wall notch (wires out side)")
    print(f"External footprint : {OUT_X1-OUT_X0:.1f} x {OUT_Y1-OUT_Y0:.1f} mm")
    print(f"External height    : {Z_LID_TOP:.1f} mm  (lid gap {UNDER_LID_GAP:.2f}mm clears U12 {TRAPPED_MAX:.2f}mm)")
    print(f"Board sits at z     = {Z_BOARD_BOT:.1f}..{Z_BOARD_TOP:.1f}  (standoff {STANDOFF_H:.0f}mm)")
    print(f"Lid underside z     = {Z_LID_UNDER:.2f}   top z={Z_LID_TOP:.2f}")
    print(f"Terminals (top-entry KF2EDGK): header {HDR_H_ABOVE_BOARD:.1f}mm + plug body {PLUG_BODY_H:.1f}mm")
    print(f"  -> mated plug top ~{Z_BOARD_TOP+HDR_H_ABOVE_BOARD+PLUG_BODY_H-HDR_H_ABOVE_BOARD:.0f}mm over board; "
          f"pokes through lid ports, wires exit up")
    print(f"Corner screw       : M3 self-tap, ~{Z_LID_TOP-(Z_BOARD_BOT-5.0):.0f} mm under head -> M3x16-20")
    print(f"Heatsink window    : {HS_X1-HS_X0:.0f} x {HS_Y1-HS_Y0:.0f} mm (PLACEHOLDER {HS_L:.0f}x{HS_W:.0f})")
    print(f"Lid windows        : {[c['name'] for c in WINDOWS]}")
    print(f"Terminal ports     : {[c['name'] for c in TERMINAL_PORTS]}")
    print(f"Logo               : {('RAISED relief ' + LOGO_SVG + f' (+{LOGO_RELIEF_H:.1f}mm proud)') if ENABLE_LOGO else 'off'}")
    print("==================================================")

main()
