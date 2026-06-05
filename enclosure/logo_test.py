#!/usr/bin/env python3
"""Fast logo-only iteration harness (no board import, no lid -> renders in seconds).
    blender --background --python logo_test.py
Prints per-curve and per-island bounding boxes so we can spot the runaway 'diagonal
bar' island, then renders the candidate relief to logo_test.png."""
import bpy, mathutils, os, math

LOGO_SVG = "riley_logo.svg"
LOGO_W   = 40.0
DILATE   = 1.1     # per-side*2 final mm (curve offset)

def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)

def wbbox(o):
    pts = [o.matrix_world @ mathutils.Vector(c) for c in o.bound_box]
    xs=[p.x for p in pts]; ys=[p.y for p in pts]
    return min(xs),max(xs),min(ys),max(ys)

def main():
    here = os.path.dirname(os.path.abspath(__file__)) if "__file__" in globals() else os.getcwd()
    reset()
    bpy.ops.import_curve.svg(filepath=os.path.join(here, LOGO_SVG))
    curves=[o for o in bpy.data.objects if o.type=='CURVE']
    # drop bg rect = largest area
    def area(o):
        x0,x1,y0,y1=wbbox(o); return (x1-x0)*(y1-y0)
    curves.sort(key=area, reverse=True)
    print("\n-- imported curves (world bbox, BEFORE offset) --")
    for i,c in enumerate(curves):
        x0,x1,y0,y1=wbbox(c)
        print(f"  [{i}] {c.name:<22} w={x1-x0:8.2f} h={y1-y0:8.2f} area={area(c):10.1f}")
    bg=curves[0]; bpy.data.objects.remove(bg, do_unlink=True)
    art=curves[1:]
    # bake import scale
    for o in bpy.context.selected_objects: o.select_set(False)
    for c in art: c.select_set(True)
    bpy.context.view_layer.objects.active=art[0]
    bpy.ops.object.transform_apply(location=False,rotation=False,scale=True)
    axs=[]
    for o in art:
        x0,x1,_,_=wbbox(o); axs+=[x0,x1]
    raw_w=(max(axs)-min(axs)) or 1.0
    s=LOGO_W/raw_w
    for c in art:
        c.data.dimensions='2D'; c.data.fill_mode='BOTH'
        c.data.offset=DILATE/(2.0*s)
    for o in bpy.context.selected_objects: o.select_set(False)
    for c in art: c.select_set(True)
    bpy.context.view_layer.objects.active=art[0]
    bpy.ops.object.convert(target='MESH')
    meshes=[o for o in art if o.type=='MESH']
    for o in bpy.context.selected_objects: o.select_set(False)
    for m in meshes: m.select_set(True)
    bpy.context.view_layer.objects.active=meshes[0]
    bpy.ops.object.join()
    logo=bpy.context.active_object; logo.name="logo"
    bpy.ops.object.transform_apply(location=True,rotation=True,scale=True)
    logo.scale=(s,s,1.0)
    bpy.ops.object.transform_apply(location=False,rotation=False,scale=True)
    # separate islands and report sizes (find the runaway bar)
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_all(action='SELECT')
    bpy.ops.mesh.separate(type='LOOSE')
    bpy.ops.object.mode_set(mode='OBJECT')
    islands=[o for o in bpy.context.selected_objects if o.type=='MESH']
    print(f"\n-- {len(islands)} islands (final mm, scaled) --")
    diags=[]
    for o in islands:
        x0,x1,y0,y1=wbbox(o); w=x1-x0; h=y1-y0; diag=math.hypot(w,h)
        diags.append((diag,w,h,o))
    diags.sort(reverse=True)
    for diag,w,h,o in diags:
        print(f"  {o.name:<14} w={w:6.2f} h={h:6.2f} diag={diag:6.2f}")
    # re-join for a quick render
    for o in bpy.context.selected_objects: o.select_set(False)
    for o in islands: o.select_set(True)
    bpy.context.view_layer.objects.active=islands[0]
    bpy.ops.object.join()
    logo=bpy.context.active_object
    sol=logo.modifiers.new(type='SOLIDIFY',name='s'); sol.thickness=1.2; sol.offset=0
    bpy.ops.object.modifier_apply(modifier=sol.name)
    # top render
    x0,x1,y0,y1=wbbox(logo); cx=(x0+x1)/2; cy=(y0+y1)/2
    cam_d=bpy.data.cameras.new("c"); cam_d.type='ORTHO'; cam_d.ortho_scale=max(x1-x0,y1-y0)*1.2
    cam=bpy.data.objects.new("c",cam_d); bpy.context.collection.objects.link(cam)
    cam.location=(cx,cy,100); cam.rotation_euler=(0,0,0)
    sc=bpy.context.scene; sc.camera=cam; sc.render.engine='BLENDER_WORKBENCH'
    sc.display.shading.show_cavity=True; sc.display.shading.show_shadows=True
    sc.render.resolution_x=900; sc.render.resolution_y=900
    sc.render.filepath=os.path.join(here,"logo_test.png")
    bpy.ops.render.render(write_still=True)
    print("wrote logo_test.png")

main()
