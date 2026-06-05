#!/usr/bin/env python3
"""
Render review images of the enclosure from assembly.blend (decoupled from the
generator so renders iterate fast).

Run:
    blender --background assembly.blend --python render_views.py
Outputs (next to assembly.blend):
    asm_iso.png   - closed box, iso (overall look + raised logo relief)
    asm_left.png  - closed box, LEFT wall (terminal lid-ports + wire-exit notches)
    lid_top.png   - closed box, straight-down on the lid (windows + logo + vents)
    board_top.png - lid hidden, straight-down on the BOARD (confirms windows line up)

Cameras are ORTHO and auto-framed from the scene bounding box (projecting the 8
bbox corners onto the camera's right/up axes), so a view can never miss the
geometry -- that was the bug that left asm_left.png blank.
"""
import bpy, mathutils, os, math

RES = 1500


def scene_bbox(skip_hidden=True):
    mn = mathutils.Vector(( 1e9,  1e9,  1e9))
    mx = mathutils.Vector((-1e9, -1e9, -1e9))
    for o in bpy.data.objects:
        if o.type != 'MESH':
            continue
        if skip_hidden and o.hide_render:
            continue
        for c in o.bound_box:
            w = o.matrix_world @ mathutils.Vector(c)
            for i in range(3):
                mn[i] = min(mn[i], w[i]); mx[i] = max(mx[i], w[i])
    return (mn + mx) * 0.5, mn, mx


def frame_camera(name, offset_dir, up_hint, margin=1.12):
    """Place an ORTHO camera looking at the scene centre from offset_dir, with
    ortho_scale sized to just contain the bbox (projected onto the view plane)."""
    center, mn, mx = scene_bbox()
    d = mathutils.Vector(offset_dir).normalized()          # camera -> target dir is -d
    up = mathutils.Vector(up_hint).normalized()
    right = d.cross(up).normalized()
    up = right.cross(d).normalized()                       # re-orthogonalise
    # project the 8 corners onto (right, up) to size the frame
    ew = eh = 0.0
    for cx in (mn.x, mx.x):
        for cy in (mn.y, mx.y):
            for cz in (mn.z, mx.z):
                w = mathutils.Vector((cx, cy, cz)) - center
                ew = max(ew, abs(w.dot(right)))
                eh = max(eh, abs(w.dot(up)))
    span = max(2 * ew, 2 * eh) * margin
    cam_data = bpy.data.cameras.new(name)
    cam_data.type = 'ORTHO'
    cam_data.ortho_scale = span
    cam_data.clip_start = 0.01
    cam_data.clip_end = 5000.0
    cam = bpy.data.objects.new(name, cam_data)
    bpy.context.collection.objects.link(cam)
    dist = max(mx - mn) * 4 + 200
    cam.location = center + d * dist
    tgt = bpy.data.objects.new(name + "_tgt", None)
    bpy.context.collection.objects.link(tgt)
    tgt.location = center
    con = cam.constraints.new('TRACK_TO')
    con.target = tgt
    con.track_axis = 'TRACK_NEGATIVE_Z'
    con.up_axis = 'UP_Y'
    return cam, tgt


def render(path, offset_dir, up_hint):
    cam, tgt = frame_camera("rc", offset_dir, up_hint)
    sc = bpy.context.scene
    sc.camera = cam
    sc.render.engine = 'BLENDER_WORKBENCH'
    sh = sc.display.shading
    sh.show_cavity = True
    sh.show_shadows = True
    sh.light = 'STUDIO'
    sc.render.resolution_x = RES
    sc.render.resolution_y = RES
    sc.render.film_transparent = False
    sc.render.filepath = path
    bpy.ops.render.render(write_still=True)
    bpy.data.objects.remove(cam, do_unlink=True)
    bpy.data.objects.remove(tgt, do_unlink=True)
    print("wrote", os.path.basename(path))


def main():
    here = os.path.dirname(bpy.data.filepath) or os.getcwd()
    lid = bpy.data.objects.get("lid")

    # closed-box views
    if lid:
        lid.hide_render = False
    render(os.path.join(here, "asm_iso.png"),  ( 1.0, -1.0, 0.75), (0, 0, 1))
    render(os.path.join(here, "asm_left.png"), (-1.0,  0.0, 0.28), (0, 0, 1))
    render(os.path.join(here, "lid_top.png"),  ( 0.0,  0.0, 1.0),  (0, 1, 0))

    # lid off -> look down on the board to confirm windows line up with components
    if lid:
        lid.hide_render = True
    render(os.path.join(here, "board_top.png"), (0.0, 0.0, 1.0), (0, 1, 0))
    if lid:
        lid.hide_render = False

    print("renders done")


main()
