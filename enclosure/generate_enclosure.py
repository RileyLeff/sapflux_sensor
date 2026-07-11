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

v0.2 BOARD / rev 7 (2026-07-11):
  * FUSE changed: the 5x20 clips are gone; F1 is now an XFCN XF-508P-B-B blade-
    fuse clip holder (16x6.7mm body, 7.35 tall) at bx8.4-15.2, by0.75-17.0.  Pad
    pitch 9.8mm = STANDARD ATO blade fuse (~19.1 wide, ~18.5 tall incl blades),
    long axis along by; the fuse pokes up through a new lid window (the holder
    itself fits under the lid).
  * Q1 moved on v0.2: courtyard now bx12.3-19.0, by35.5-44.4 -> heatsink window
    recentred at (15.7, 39.9).
  * Terminal lid PORTS deepened: the KF2EDGK plug TOP is 15.7mm deep vs the 8mm
    header (datasheet), overhanging ~8.7mm INBOARD (toward the PCB) -> back face
    ~bx16.7.  Old ports ended at bx10 and fouled the plug tops; now PORT_X1=18.
  * ZIP-TIE STRAP LOOPS on the left+right (bx) walls for lashing the box to a
    tree: staple-style tunnels flush with the back face (print flat, no
    supports).  Mount the box long-axis-vertical; each tie runs through the two
    collinear tunnels on one end wall and wraps the trunk just beyond the box.
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
# --- base post "volcano" (flared root) ----------------------------------
# The nut pocket (r3.3) is wider than the post (r3.0), so on the base the post was sheared
# off the floor by a 0.3mm ring (printed as an island, same failure as the lid).  Unlike the
# lid there are NO windows near the base corners, so here we CAN reconnect outboard: flare
# each post into the floor with a cone that is fat (r=BASE_FLARE_R) at the floor and tapers
# to the post radius by BASE_FLARE_TOP_Z.  The cone roots OUTSIDE the nut pocket (r3.3) so it
# bridges the gap; it also prints self-supporting (each layer narrower going up) and buttresses
# into the corner walls.  Top stays below the board (z6.5) so it never lifts the PCB.
BASE_FLARE_R     = 5.0    # cone radius where it meets the floor (well past the r3.3 pocket)
BASE_FLARE_TOP_Z = 4.5    # cone tapers back to POST_OD by here (2.0mm under the board)
SCREW_CLR_D  = 3.4        # M3 clearance hole through BOTH lid pillars and base posts
                         # (the screw no longer threads into plastic - see nut trap).
# --- captive nut (user owns M3 machine screws + M3 nuts + M3 washers, no self-tappers).
# A machine screw can't bite bare PETG reliably, so each base post gets a hex pocket at
# its BOTTOM face that captures a standard M3 nut; the screw drops in from the lid and
# threads into the steel nut.  Far stronger + reusable vs thread-forming into plastic.
NUT_AF       = 5.5        # M3 hex nut across-flats (DIN934)
NUT_POCKET_D = 6.6        # hex pocket across-CORNERS (add_cyl verts=6 dia). AF=6.6*cos30
                         # =5.72mm -> ~0.2mm light press-fit on the 5.5mm nut.
HEAD_CB_D    = 7.5        # counterbore widened 6.5->7.5 to seat an M3 washer (OD ~7mm)
# HEAD_CB_DEPTH 2.6 -> 1.6: the old 2.6 bored the counterbore (r3.75) clean THROUGH the
# 2.5mm lid plate (+0.1 into the pillar), so the r3.0 pillar was severed from the plate by
# a 0.75mm ring -> it printed as a floating island on the supports.  We CANNOT reconnect
# outboard (a ring past r3.75 would hit the terminal ports / LED, all <=3.54mm from a
# corner), so we leave a solid WEB under the counterbore: depth 1.6 keeps a 0.9mm-thick
# plate floor (r up to 3.75) that the pillar joins, reconnecting it firmly.  Head now sits
# ~1mm proud; pair with M3x18 for full captive-nut engagement (tip flush at box bottom).
HEAD_CB_DEPTH= 1.6        # leaves a 0.9mm connecting web (LID 2.5 - 1.6) under the bore

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
PLUG_TOP_DEPTH    = 15.7  # KF2EDGK plug TOP depth (datasheet); header is only 8.0 deep
                          # (bx1..9), so the screw-top overhangs INBOARD to ~bx16.7.
PORT_X1           = 18.0  # terminal lid ports reach this far inboard: 16.7 + 1.3 clearance
                          # (rev-6 ports ended at bx10 and the plug tops fouled the lid).

# --- zip-tie strap loops (rev 7): lash the box to a tree ----------------
# Two staple-style loops on EACH bx end wall (left + right), flush with the back
# face so the base still prints flat with zero supports.  A tie slides through the
# two collinear tunnels of one wall (lying flat against the bark through the open
# tunnel bottoms) and wraps the trunk just beyond that end of the box -> mount the
# box long-axis-vertical and use one tie per end.  Tension presses the loop (and
# box) against the bark; the roof only sees pull-away load.
ZIP_TIE_W    = 10.0       # max tie width (9mm heavy-duty tie + slack); tunnel clear = +1.4
ZIP_TIE_T    = 3.6        # tunnel height (HD tie ~2.5 thick + slack)
ZIP_ROOF     = 3.2        # roof thickness over the tunnel (bridges 11.4mm when printing - fine)
ZIP_LEG      = 3.4        # solid outer leg beyond the tunnel
ZIP_LOOP_YS  = [(10.0, 20.0), (45.0, 55.0)]   # tunnel spans along the wall; the pair is
                          # symmetric about by32.5 so the board mirror can't misplace them.
                          # Loop top = ZIP_TIE_T+ZIP_ROOF = 6.8 -- MUST stay below the wall
                          # wire-notch floor (Z_BOARD_TOP-1 = 7.1) on the left wall.

ENABLE_VENTS = True
VENT_D       = 4.0
VENT_PITCH   = 9.0

# --- Q1 heatsink bounding box (PLACEHOLDER - set to your real heatsink!) ---
# v0.2: Q1 moved; courtyard is now bx12.3-19.0, by35.5-44.4 (was centred ~34.2).
HS_CENTER_BX = 15.7
HS_CENTER_BY = 39.9
HS_L         = 13.0       # along bx  (placeholder heatsink)
HS_W         = 10.0       # along by  (placeholder heatsink)
HS_WIN_MARGIN= 2.0        # 2.0 (was 1.5) so the window's y0=32.9 OVERLAPS the heater port
                          # (y1=33.3) -- a near-tangent gap would leave an unprintable
                          # 0.1mm lid sliver between the two cuts.  With v0.2's Q1 position
                          # the window also overlaps the rs485 port -> the heater port, Q1
                          # window and rs485 port merge into one big west-side opening.

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
    dict(name="top_io", x0=29.0, x1=83.0, y0=-1.0, y1=6.0,  r=0.0),  # SWD + aux + test pads (edge rim 1->2mm)
    # v0.2 blade fuse: ATO fuse body bx8.95-14.65, by-0.7-18.4 (overhangs the 16mm holder
    # in by), pokes up through the lid.  x0=7.4 is pillar-limited: the (4,4) corner pillar
    # (r3.0) reaches bx7.0 -> 0.4mm clearance.  Merges with the t_power port (they overlap)
    # into one opening over the power corner -> finger room to pull the fuse.
    dict(name="fuse",   x0=7.4,  x1=17.2, y0=-1.0, y1=20.0, r=3.0),  # ATO blade fuse in F1 holder
    # cr2032 + logo are ONE flowing opening: the two rects OVERLAP (bx74-76 over ~19mm of
    # by) so they merge into a single cutout that both frames the riley logo and opens up
    # the coin for finger access -- no dividing rib.
    dict(name="cr2032", x0=74.0, x1=99.0, y0=9.0, y1=50.0, r=6.0),  # coin access (edge x1=99 kept), reaches left to merge with logo
    dict(name="logo",   x0=51.0, x1=76.0, y0=5.0, y1=28.0, r=4.0),  # frames silkscreen line-art, reaches right to merge with cr2032
    dict(name="ribbon", x0=10.0, x1=88.0, y0=56.0, y1=66.0, r=0.0),  # 60-pin + IDC ribbon (edge rim 1->2mm)
    dict(name="led",    x0=90.0, x1=93.5, y0=55.0, y1=58.5, r=1.5),  # U10 RGB status LED (bx92.3,by57.2)
    dict(name="q1",     x0=HS_X0, x1=HS_X1, y0=HS_Y0, y1=HS_Y1, r=3.0),  # heatsink (smaller, PLACEHOLDER)
]
# Terminal I/O-shield ports (top-entry plugs pass straight UP through these).
# rev 7: x1 = PORT_X1 (18.0, was 10.0) because the plug screw-tops overhang inboard to
# ~bx16.7 (see PLUG_TOP_DEPTH).  y widened toward the headers where the corner pillars
# allow: t_power y0 and t_rs485 y1 stop 0.3-0.4mm short of the r3.0 pillars at (4,4)/(4,61).
TERMINAL_PORTS = [
    dict(name="t_power",  x0=IN_X0, x1=PORT_X1, y0=7.4,  y1=17.5, r=2.0),  # CN1 2P (hdr by7.95-15.95)
    dict(name="t_heater", x0=IN_X0, x1=PORT_X1, y0=22.3, y1=33.3, r=2.0),  # J1  2P (hdr by23.8-31.8)
    dict(name="t_rs485",  x0=IN_X0, x1=PORT_X1, y0=44.2, y1=57.7, r=2.0),  # J3  3P (hdr by45.71-57.21)
]
LID_CUTS = WINDOWS + TERMINAL_PORTS

# --- BOARD-FRAME MIRROR (real root cause of the first-print mirror) ----
# CONFIRMED by reading the rendered silkscreen (it came out BACKWARDS): the KiCad glb
# import used scale.y = -f, which is a REFLECTION, not a rotation.  Every window/port/
# notch in this file is written in that reflected "by = Yk-85" frame, so the first print
# came out as a MIRROR of the real board along the terminal axis (printed 2,2,3 but the
# board reads 3,2,2 from the CR2032 side).  import_board() is now fixed to show the board
# in its TRUE orientation (silkscreen reads forward); to match it, every ASYMMETRIC cut
# -- the lid windows/ports AND the base wire-notches -- is mirrored in by here
# (by -> BOARD_H-by).  Corner posts/holes are by-symmetric, so they are unaffected.
MIRROR_BOARD_BY = True
def _mirror_cut(c):
    if not MIRROR_BOARD_BY:
        return c
    d = dict(c)
    d["y0"], d["y1"] = BOARD_H - c["y1"], BOARD_H - c["y0"]
    return d
# Cuts as actually subtracted, in the true (real-board) frame:
LID_CUTS_LID = [_mirror_cut(c) for c in LID_CUTS]

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

def add_cone(name, x, y, z0, z1, d0, d1, verts=64):
    """Frustum: diameter d0 at z0 (bottom) tapering to d1 at z1 (top).  Same vertex count
    as add_cyl so a coaxial cone+cylinder union shares facets cleanly (no sliver ring)."""
    bpy.ops.mesh.primitive_cone_add(vertices=verts, radius1=d0/2.0, radius2=d1/2.0,
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
        # "volcano": flare the post root into the floor so the nut pocket (r3.3 > post r3.0)
        # can't shear it off.  Cone roots at z=Z_FLOOR_TOP-0.6 (inside the floor, welds to it)
        # and tapers to POST_OD by BASE_FLARE_TOP_Z.  Unioned BEFORE the pocket is bored, so
        # the pocket re-clears the nut bore through it -> a 1.2mm solid ring bridges post->floor.
        flare = add_cone(f"flare{i}", cx, cy, Z_FLOOR_TOP - 0.6, BASE_FLARE_TOP_Z,
                         2*BASE_FLARE_R, POST_OD)
        boolean(base, flare, 'UNION')
    # NUT TRAP per corner, each cutter subtracted INDIVIDUALLY: (1) a 3.4mm clearance hole
    # up through the post so the screw passes freely, (2) a hex pocket open at the BOTTOM
    # face (z<=floor top) that captures a press-fit M3 nut.  Shoulder = floor top, so the
    # pocket lives entirely in the full-width backplate (the freestanding post above z=floor
    # is NOT necked).  The screw draws the nut up against this shoulder; tip stays inside the
    # pocket (no protrusion).  NB the clr (z2.5+) and nut (z<=2.5) stack and touch at z2.5 --
    # JOINING them makes coincident coplanar faces that EXACT subtracts only partially (see
    # napkin), so they are subtracted one at a time, never joined.
    for i, (cx, cy) in enumerate(CORNERS):
        # clr starts 0.5mm BELOW the shoulder so it OVERLAPS the hex pocket -> the two holes
        # form one clean volume with no coincident plane at z2.5 (a coincident clr-floor/nut-
        # ceiling left a zero-thickness membrane that blocked the bore).  The nut SEAT shoulder
        # is the z2.5 annulus r1.7-2.86 (clr only removes r<1.7, so the seat is untouched).
        boolean(base, add_cyl(f"clr{i}", cx, cy, Z_FLOOR_TOP - 0.5, Z_BOARD_BOT + 0.1, SCREW_CLR_D), 'DIFFERENCE')
        boolean(base, add_cyl(f"nut{i}", cx, cy, -0.1, Z_FLOOR_TOP, NUT_POCKET_D, verts=6), 'DIFFERENCE')
    # left-wall wire-exit notches, directly below each lid port (open corner).
    # Mirror in by like the lid cuts so the notches line up with the REAL terminals.
    for c in TERMINAL_PORTS:
        cm = _mirror_cut(c)
        boolean(base, add_box("wn_"+cm["name"], OUT_X0 - 1.0, IN_X0 + 1.0, cm["y0"], cm["y1"],
                              Z_BOARD_TOP - 1.0, Z_LID_UNDER + 0.5), 'DIFFERENCE')
    # zip-tie strap loops (rev 7), two per bx end wall.  Each loop: a block rooted 0.3mm
    # INTO the wall face (welds to wall AND backplate edge), with an open-bottomed tunnel
    # bored along y -> a "staple" the tie slides through while lying flat on the bark.
    # The tunnel cutter reaches 0.4mm INTO the wall (no coincident-plane boolean, see
    # napkin) and 1mm past both y ends so the ends are fully open.  Loop top (6.8) sits
    # below the left wall's wire-notch floor (7.1), so loops and notches never interact.
    for (ly0, ly1) in ZIP_LOOP_YS:
        for tag, wf, sgn in (("L", OUT_X0, -1.0), ("R", OUT_X1, +1.0)):
            xa = wf - sgn * 0.3
            xb = wf + sgn * (1.4 + ZIP_TIE_W + ZIP_LEG)
            boolean(base, add_box(f"zl_{tag}_{ly0:.0f}", min(xa, xb), max(xa, xb),
                                  ly0, ly1, 0.0, ZIP_TIE_T + ZIP_ROOF), 'UNION')
            ta = wf - sgn * 0.4          # 0.4 into the wall
            tb = wf + sgn * (1.4 + ZIP_TIE_W)
            boolean(base, add_box(f"zt_{tag}_{ly0:.0f}", min(ta, tb), max(ta, tb),
                                  ly0 - 1.0, ly1 + 1.0, -0.1, ZIP_TIE_T), 'DIFFERENCE')
    clean_mesh(base)   # weld slivers from the coaxial cone+post unions -> manifold STL
    base.name = "base"
    return base

def _in_any_cut(x, y, m=3.0):
    for c in LID_CUTS_LID:
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
    for c in LID_CUTS_LID:
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
    clean_mesh(lid)   # weld CSG facet slivers (e.g. counterbore rims) -> manifold STL
    lid.name = "lid"
    return lid

def clean_mesh(obj):
    """Weld coincident verts and fill any tiny boundary slivers left by the EXACT booleans
    (a faceted counterbore rim can leave a sub-mm non-manifold sliver).  Conservative: a
    1e-3 merge distance only collapses true duplicates, and fill_holes(sides=8) only closes
    loops up to 8 edges -- the window openings are proper manifold holes (vertical walls),
    not boundary loops, so they are untouched."""
    bpy.context.view_layer.objects.active = obj
    for o in bpy.context.selected_objects:
        o.select_set(False)
    obj.select_set(True)
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_all(action='SELECT')
    bpy.ops.mesh.remove_doubles(threshold=1e-3)
    bpy.ops.mesh.select_all(action='DESELECT')
    bpy.ops.mesh.select_non_manifold()
    bpy.ops.mesh.fill_holes(sides=8)
    bpy.ops.mesh.normals_make_consistent(inside=False)
    bpy.ops.object.mode_set(mode='OBJECT')

# ======================================================================
def count_shells(obj):
    """Number of connected mesh islands.  THE definitive test for the detached-post bug:
    a correct one-piece part is a single shell; every post that floats free of the body
    adds another shell (e.g. body + 4 islands = 5).  Flood-fill the vertex/edge graph."""
    bm = bmesh.new(); bm.from_mesh(obj.data)
    seen = set(); shells = 0
    for v0 in bm.verts:
        if v0.index in seen:
            continue
        shells += 1
        stack = [v0]
        while stack:
            v = stack.pop()
            if v.index in seen:
                continue
            seen.add(v.index)
            for e in v.link_edges:
                o = e.other_vert(v)
                if o.index not in seen:
                    stack.append(o)
    bm.free()
    return shells

def verify_holes():
    lid = bpy.data.objects.get("lid")
    deps = bpy.context.evaluated_depsgraph_get()
    # (name, x, y, want_solid, z_start, dist)
    checks = [(c["name"], (c["x0"]+c["x1"])/2, (c["y0"]+c["y1"])/2, False, Z_LID_TOP + 0.5, 3.0)
              for c in LID_CUTS_LID]
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
    # --- base NUT TRAP verification (one representative corner).  Rays START IN AIR (above
    # the post or below the open bottom face) and cross toward the feature: a ray that begins
    # inside solid would cross no face and falsely read OPEN. ---
    base = bpy.data.objects.get("base")
    if base is not None:
        cx, cy = CORNERS[0]
        ro = 2.2   # offset radius: inside the hex pocket (flat 2.86) & post (3.0), outside clr (1.7)
        DN, UP = mathutils.Vector((0,0,-1)), mathutils.Vector((0,0,1))
        # (name, x, y, dir, z_start, dist, want_solid)
        bchecks = [
            ("clr_axis_open", cx,    cy, DN, Z_BOARD_BOT + 0.5, 9.0, False),  # screw bore runs through
            ("post_wall",     cx+ro, cy, DN, Z_BOARD_BOT + 0.5, 1.0, True),   # post solid beside the bore
            ("nut_seat",      cx+ro, cy, UP, -1.0,              5.0, True),    # shoulder ceiling at z2.5 exists
            ("pocket_open",   cx,    cy, UP, -1.0,              5.0, False),   # axis clear from the bottom up
        ]
        print("-- base nut-trap verification (corner %s) --" % (CORNERS[0],))
        for name, x, y, d, z0, dist, want_solid in bchecks:
            hit, *_ = base.ray_cast(mathutils.Vector((x, y, z0)), d, depsgraph=deps, distance=dist)
            ok = (hit == want_solid)
            print(f"   {name:<14} {'SOLID' if hit else 'OPEN '}  {'ok' if ok else '*** UNEXPECTED ***'}")
        # FLARE bridge: a horizontal ray inward at z3.5 must strike the cone OUTSIDE the old
        # post radius (~3.0).  A hit at radius >3.3 proves the volcano spans the nut-pocket gap
        # (without it the ray would hit the bare post at r3.0 -> floating island).
        cx0, cy0 = CORNERS[0]   # (4,4): clear cavity to its +x, ray travels -x
        hit, loc, *_ = base.ray_cast(mathutils.Vector((cx0 + 9.0, cy0, 3.5)),
                                     mathutils.Vector((-1, 0, 0)), depsgraph=deps, distance=9.0)
        rad = (loc.x - cx0) if hit else float('nan')
        print(f"   flare_bridge   {'SOLID' if hit else 'OPEN '} at r={rad:.2f}mm  "
              f"{'ok (spans pocket r3.3)' if hit and rad > 3.3 else '*** UNEXPECTED ***'}")
        # ZIP strap loops: for each loop, a y-ray through the tunnel must pass clean
        # (open ends, open along the full span), and down-rays must strike the roof and
        # the outer leg (solid, i.e. the staple actually exists and is attached).
        print("-- zip-tie strap loop verification --")
        for (ly0, ly1) in ZIP_LOOP_YS:
            for tag, wf, sgn in (("L", OUT_X0, -1.0), ("R", OUT_X1, +1.0)):
                xt = wf + sgn * (1.4 + ZIP_TIE_W) / 2.0          # mid-tunnel x
                xl = wf + sgn * (1.4 + ZIP_TIE_W + ZIP_LEG/2.0)  # mid-leg x
                hit, *_ = base.ray_cast(mathutils.Vector((xt, ly0 - 3.0, ZIP_TIE_T/2.0)),
                                        mathutils.Vector((0, 1, 0)), depsgraph=deps,
                                        distance=(ly1 - ly0) + 6.0)
                print(f"   zt_{tag}_{ly0:.0f} tunnel  {'SOLID' if hit else 'OPEN '}  "
                      f"{'*** BLOCKED ***' if hit else 'ok'}")
                for nm, x in (("roof", xt), ("leg ", xl)):
                    hit, *_ = base.ray_cast(mathutils.Vector((x, (ly0+ly1)/2.0, ZIP_TIE_T + ZIP_ROOF + 2.0)),
                                            DN, depsgraph=deps, distance=4.0)
                    print(f"   zl_{tag}_{ly0:.0f} {nm}    {'SOLID' if hit else 'OPEN '}  "
                          f"{'ok' if hit else '*** MISSING ***'}")
    # --- ISLAND CHECK: the whole point of this revision.  Each part must be ONE shell. ---
    print("-- shell count (1 = fully connected, >1 = detached islands) --")
    for nm in ("base", "lid"):
        ob = bpy.data.objects.get(nm)
        if ob is not None:
            n = count_shells(ob)
            print(f"   {nm:<14} shells={n}  {'ok' if n == 1 else '*** DETACHED ISLANDS ***'}")

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
    # IMPORTANT: scale.y must be POSITIVE.  A negative single-axis scale is a REFLECTION
    # (mirror image -> silkscreen reads backwards) and was the root cause of the first
    # print's mirrored cutouts.  Use a proper scale and put the board in its true
    # orientation by reflecting through position instead: by_scene = (BOARD_H+85) - Yk.
    emp.scale = (factor, factor, factor)
    emp.location = (-100.0, BOARD_H + 85.0, Z_BOARD_BOT)
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
    zip_ext = 1.4 + ZIP_TIE_W + ZIP_LEG
    print(f"External footprint : {OUT_X1-OUT_X0:.1f} x {OUT_Y1-OUT_Y0:.1f} mm "
          f"(+{zip_ext:.1f}mm zip loops each bx end -> {OUT_X1-OUT_X0+2*zip_ext:.1f}mm total; needs a >=140mm bed in x)")
    print(f"Zip-tie loops      : 2 per bx end wall at by{ZIP_LOOP_YS[0][0]:.0f}-{ZIP_LOOP_YS[0][1]:.0f}/"
          f"{ZIP_LOOP_YS[1][0]:.0f}-{ZIP_LOOP_YS[1][1]:.0f}; tunnel {1.4+ZIP_TIE_W:.1f}w x {ZIP_TIE_T:.1f}h "
          f"(ties up to ~{ZIP_TIE_W:.0f}mm wide); one tie per end, box long-axis vertical on the trunk")
    print(f"Terminal ports     : reach inboard to bx{PORT_X1:.0f} (plug top {PLUG_TOP_DEPTH:.1f} deep -> back ~bx16.7)")
    print(f"Fuse               : v0.2 ATO blade fuse pokes through lid window (holder 7.35 tall fits under lid)")
    print(f"External height    : {Z_LID_TOP:.1f} mm  (lid gap {UNDER_LID_GAP:.2f}mm clears U12 {TRAPPED_MAX:.2f}mm)")
    print(f"Board sits at z     = {Z_BOARD_BOT:.1f}..{Z_BOARD_TOP:.1f}  (standoff {STANDOFF_H:.0f}mm)")
    print(f"Lid underside z     = {Z_LID_UNDER:.2f}   top z={Z_LID_TOP:.2f}")
    print(f"Terminals (top-entry KF2EDGK): header {HDR_H_ABOVE_BOARD:.1f}mm + plug body {PLUG_BODY_H:.1f}mm")
    print(f"  -> mated plug top ~{Z_BOARD_TOP+HDR_H_ABOVE_BOARD+PLUG_BODY_H-HDR_H_ABOVE_BOARD:.0f}mm over board; "
          f"pokes through lid ports, wires exit up")
    print(f"Corner screw       : M3x18 machine screw + M3 washer (top) -> captive M3 nut in base post")
    print(f"  nut trap          : hex pocket d{NUT_POCKET_D}mm (AF {NUT_POCKET_D*math.cos(math.radians(30)):.2f}) at post bottom, press-fit; M3x18 = full nut engage, tip flush at box bottom")
    print(f"  post reconnect     : lid pillar via {LID-HEAD_CB_DEPTH:.1f}mm web under counterbore; base post via r{BASE_FLARE_R:.0f} flared 'volcano' root")
    print(f"Heatsink window    : {HS_X1-HS_X0:.0f} x {HS_Y1-HS_Y0:.0f} mm (PLACEHOLDER {HS_L:.0f}x{HS_W:.0f})")
    print(f"Lid windows        : {[c['name'] for c in WINDOWS]}")
    print(f"Terminal ports     : {[c['name'] for c in TERMINAL_PORTS]}")
    print(f"Logo               : {('RAISED relief ' + LOGO_SVG + f' (+{LOGO_RELIEF_H:.1f}mm proud)') if ENABLE_LOGO else 'off'}")
    print("==================================================")

main()
