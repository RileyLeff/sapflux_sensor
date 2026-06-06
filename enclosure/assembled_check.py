#!/usr/bin/env python3
"""Top-down 'as-assembled' check: translucent lid dropped straight onto the real board
(NO flips -- the board is imported in its true orientation and the lid cuts are in the
same real frame).  Every window/port should sit over its component.
   blender --background assembly.blend --python assembled_check.py -> assembled_check.png"""
import bpy, os
here = os.path.dirname(bpy.data.filepath) or os.getcwd()
lid = bpy.data.objects.get("lid"); base = bpy.data.objects.get("base")
if base: base.hide_render = True            # hide base walls so we see the board clearly
# translucent blue lid
m = bpy.data.materials.new("lm"); m.use_nodes=True
b = m.node_tree.nodes.get("Principled BSDF")
b.inputs["Base Color"].default_value=(0.10,0.28,0.85,1.0); b.inputs["Alpha"].default_value=0.40
try: m.surface_render_method='BLENDED'
except Exception: pass
try: m.blend_method='BLEND'
except Exception: pass
lid.data.materials.clear(); lid.data.materials.append(m)
cd=bpy.data.cameras.new("c"); cd.type='ORTHO'; cd.ortho_scale=116
cam=bpy.data.objects.new("c",cd); bpy.context.collection.objects.link(cam)
cam.location=(50,32.5,150); cam.rotation_euler=(0,0,0)
sc=bpy.context.scene; sc.camera=cam
s=bpy.data.lights.new("s",'SUN'); s.energy=5
so=bpy.data.objects.new("s",s); bpy.context.collection.objects.link(so); so.rotation_euler=(0.25,0.15,0)
w=bpy.data.worlds.new("w"); sc.world=w; w.use_nodes=True
w.node_tree.nodes["Background"].inputs[0].default_value=(0.86,0.86,0.86,1)
for e in ('BLENDER_EEVEE_NEXT','BLENDER_EEVEE'):
    try: sc.render.engine=e; break
    except Exception: pass
sc.render.resolution_x=1600; sc.render.resolution_y=1050
sc.render.filepath=os.path.join(here,"assembled_check.png")
bpy.ops.render.render(write_still=True)
print("wrote assembled_check.png")
