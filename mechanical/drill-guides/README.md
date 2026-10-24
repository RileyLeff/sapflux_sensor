# Sap Flux Drill Guides

Reversible, flat drill guides for three-needle East30 sensor heads. Each
spacing is exported for both number 54 and number 55 drill bits:

- `equidistant-6mm-bit-54.stl`
- `equidistant-6mm-bit-55.stl`
- `riley-10mm-5mm-bit-54.stl`
- `riley-10mm-5mm-bit-55.stl`

The equidistant guides place the holes 6 mm center-to-center on both sides of
the heater. The Riley guides place them 10 mm and 5 mm apart.

All guides are 12 mm wide and 3 mm thick, with 5 mm of material beyond the
outer holes. With a drill bit 13 mm longer than the sensor needles, the 3 mm
guide leaves approximately 10 mm available for chuck engagement while retaining
the full needle-length drilling depth.

The number 55 exports use 1.80 mm CAD guide holes for a 1.321 mm bit. The
number 54 exports use 1.90 mm CAD guide holes for a 1.397 mm bit. These include
compensation for the first printer test, where nominal 1.6 mm holes printed too
small for the sensor needles. Keep slicer X-Y/hole compensation at zero for the
first print of these revised files.

Recommended FDM settings:

- Print flat at 100% scale with no supports.
- Use a 0.4 mm nozzle and 0.16 or 0.20 mm layers.
- Use three walls/perimeters and 100% infill.
- Use normal or Arachne wall generation; wall count should not alter the
  modeled bore diameter.
- Print external and hole perimeters slowly, around 20-30 mm/s.
- Check the sliced preview and confirm all three bores remain open through every
  layer before starting the print.

After printing, test the intended bit by hand. It should slide through without
force but should not rattle significantly. If needed, clear the hole gently
with that same bit at low speed; high-speed rubbing can melt the guide.

Needle diameter conversion:

- 1.27 mm = 0.0500 inch = exactly 1/20 inch.
- 1.30 mm = 0.0512 inch.
- A number 55 drill is 0.0520 inch (1.321 mm).
- A number 54 drill is 0.0550 inch (1.397 mm).
