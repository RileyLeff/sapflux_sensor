#!/usr/bin/env python3
"""Exploded 'translucent PETG' beauty render: base + board + lid stacked apart,
the base & lid in see-through water-bottle-tinted plastic so the board shows through.
    blender --background assembly.blend --python render_exploded.py
Output: exploded.png  (next to assembly.blend)
Uses EEVEE so alpha-blended plastic actually renders transparent (Workbench can't)."""
import bpy, mathutils, math, os

RES        = 1600
LID_RISE   = 62.0    # how far to float the lid above its closed position (mm)
BOARD_RISE = 36.0    # float the board well ABOVE the base walls so the translucent
                     # front wall no longer sits in front of (and blue-tints) the board
TINT       = (0.06, 0.20, 0.72, 1.0)   # translucent BLUE plastic
ALPHA      = 0.74    # much less see-through now (still reads through the flat faces)
ROUGH      = 0.12
PCB_GREEN  = (0.02, 0.26, 0.07, 1.0)   # classic soldermask green for the board body

# ---------------------------------------------------------------- materials
def petg_material():
    m = bpy.data.materials.new("PETG_translucent")
    m.use_nodes = True
    nt = m.node_tree
    bsdf = nt.nodes.get("Principled BSDF")
    bsdf.inputs["Base Color"].default_value = TINT
    bsdf.inputs["Roughness"].default_value = ROUGH
    if "IOR" in bsdf.inputs:
        bsdf.inputs["IOR"].default_value = 1.5
    # Fresnel-driven alpha: face-on = ALPHA (see-through), grazing edges -> ~1 (frosted
    # rim), so the shell reads as real translucent plastic instead of vanishing.
    fres = nt.nodes.new("ShaderNodeFresnel"); fres.inputs["IOR"].default_value = 1.45
    mx = nt.nodes.new("ShaderNodeMath"); mx.operation = 'MAXIMUM'
    mx.inputs[1].default_value = ALPHA
    nt.links.new(fres.outputs["Fac"], mx.inputs[0])
    if "Alpha" in bsdf.inputs:
        nt.links.new(mx.outputs["Value"], bsdf.inputs["Alpha"])
    # alpha-blend so we see straight through (works on both EEVEE legacy + Next)
    try:    m.surface_render_method = 'BLENDED'      # EEVEE Next (4.2+/5.x)
    except Exception: pass
    try:    m.blend_method = 'BLEND'                 # EEVEE legacy
    except Exception: pass
    try:    m.show_transparent_back = False
    except Exception: pass
    return m

def apply_material(obj, mat):
    obj.data.materials.clear()
    obj.data.materials.append(mat)
    try:    obj.visible_shadow = False              # don't drop a hard shadow on the board
    except Exception: pass

def recolor_board_green(board):
    """The KiCad glb imports the PCB body pale; repaint the board SLAB(s) green.
    The slab is any board-descendant mesh whose footprint covers most of the 100x65
    outline -- components are small-footprint so they keep their own colours."""
    if not board:
        return
    def descend(o):
        r = []
        for c in o.children:
            r.append(c); r += descend(c)
        return r
    n = 0
    for o in descend(board):
        if o.type != 'MESH':
            continue
        pts = [o.matrix_world @ mathutils.Vector(c) for c in o.bound_box]
        w = max(p.x for p in pts) - min(p.x for p in pts)
        h = max(p.y for p in pts) - min(p.y for p in pts)
        if w > 80 and h > 45:                       # the board substrate, not a part
            if not o.data.materials:
                o.data.materials.append(bpy.data.materials.new("pcb_green"))
            for mm in o.data.materials:
                if not mm:
                    continue
                mm.use_nodes = True
                b = mm.node_tree.nodes.get("Principled BSDF")
                if b:
                    b.inputs["Base Color"].default_value = PCB_GREEN
                    b.inputs["Roughness"].default_value = 0.45
            n += 1
    print(f"recolored {n} board-slab mesh(es) green")

# ---------------------------------------------------------------- framing
def scene_bbox():
    mn = mathutils.Vector(( 1e9,)*3); mx = mathutils.Vector((-1e9,)*3)
    for o in bpy.data.objects:
        if o.type != 'MESH' or o.hide_render:
            continue
        for c in o.bound_box:
            w = o.matrix_world @ mathutils.Vector(c)
            for i in range(3):
                mn[i] = min(mn[i], w[i]); mx[i] = max(mx[i], w[i])
    return (mn + mx) * 0.5, mn, mx

def frame_camera(offset_dir, up_hint, margin=1.35, vshift_frac=0.15):
    """ORTHO camera auto-framed to the scene bbox.  The camera orientation is set
    DIRECTLY (cols = right/up/forward) rather than via a TRACK_TO constraint, so the
    height estimate is exact and the framing can't clip.  `margin` is breathing room;
    `vshift_frac` pans the look-at UP by that fraction of the FRAME height (so content
    drops that much in the image) -> headroom for the floated lid at the top."""
    center, mn, mx = scene_bbox()
    d = mathutils.Vector(offset_dir).normalized()        # camera sits at center + d*dist
    up = mathutils.Vector(up_hint).normalized()
    # right-handed basis with local +Z = d (camera looks down -Z): right = up x d, up = d x right.
    # (d x up here would flip handedness -> a mirrored render.)
    right = up.cross(d).normalized()
    up = d.cross(right).normalized()
    ew = eh = 0.0
    for cx in (mn.x, mx.x):
        for cy in (mn.y, mx.y):
            for cz in (mn.z, mx.z):
                w = mathutils.Vector((cx, cy, cz)) - center
                ew = max(ew, abs(w.dot(right))); eh = max(eh, abs(w.dot(up)))
    frame = max(2*ew, 2*eh) * margin                     # = ortho_scale (full frame size)
    framed = center + up * (vshift_frac * frame)         # pan look-at up by a fraction of the FRAME
    dist = max(mx - mn) * 5 + 300
    loc = framed + d * dist
    cd = bpy.data.cameras.new("cam"); cd.type = 'ORTHO'
    cd.ortho_scale = frame
    cd.clip_start = 0.01; cd.clip_end = 8000.0
    cam = bpy.data.objects.new("cam", cd); bpy.context.collection.objects.link(cam)
    # camera local axes: +X=right, +Y=up, +Z=forward (looks down -Z toward the scene)
    cam.matrix_world = mathutils.Matrix((
        (right.x, up.x, d.x, loc.x),
        (right.y, up.y, d.y, loc.y),
        (right.z, up.z, d.z, loc.z),
        (0.0,     0.0,  0.0, 1.0),
    ))
    return cam

# ---------------------------------------------------------------- main
def main():
    here = os.path.dirname(bpy.data.filepath) or os.getcwd()
    base  = bpy.data.objects.get("base")
    lid   = bpy.data.objects.get("lid")
    board = bpy.data.objects.get("BOARD")

    mat = petg_material()
    if base: apply_material(base, mat)
    if lid:  apply_material(lid, mat)
    recolor_board_green(board)

    # explode along +Z: base stays, board floats up, lid floats highest
    if board: board.location.z += BOARD_RISE
    if lid:   lid.location.z   += LID_RISE

    # lights
    sun_d = bpy.data.lights.new("sun", 'SUN'); sun_d.energy = 4.0
    sun = bpy.data.objects.new("sun", sun_d); bpy.context.collection.objects.link(sun)
    sun.rotation_euler = (math.radians(52), math.radians(8), math.radians(35))
    world = bpy.data.worlds.new("w"); bpy.context.scene.world = world
    world.use_nodes = True
    # warm sand background -> strong contrast against the blue plastic
    world.node_tree.nodes["Background"].inputs[0].default_value = (0.80, 0.66, 0.45, 1.0)
    world.node_tree.nodes["Background"].inputs[1].default_value = 1.0

    cam = frame_camera((1.0, -1.0, 0.62), (0, 0, 1))
    sc = bpy.context.scene
    sc.camera = cam
    # EEVEE so alpha/transmission render (engine id differs across versions)
    for eng in ('BLENDER_EEVEE_NEXT', 'BLENDER_EEVEE'):
        try:
            sc.render.engine = eng; break
        except Exception:
            continue
    try:    sc.eevee.use_raytracing = True       # nicer transmission on EEVEE Next
    except Exception: pass
    try:    sc.eevee.taa_render_samples = 64
    except Exception: pass
    sc.render.resolution_x = RES; sc.render.resolution_y = RES
    sc.render.film_transparent = False
    sc.render.filepath = os.path.join(here, "exploded.png")
    bpy.ops.render.render(write_still=True)
    print("wrote exploded.png  (engine:", sc.render.engine, ")")

main()
