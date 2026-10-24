// Flat three-hole drill guide for East30 sap-flux sensor needles.
// Override style or bit_size from the OpenSCAD command line as needed.

style = "equidistant"; // "equidistant" or "riley"
bit_size = 55;         // 54 (1.397 mm) or 55 (1.321 mm)
guide_width = 12;
guide_thickness = 3;
edge_margin = 5;

// These CAD diameters include compensation for the first print's undersized
// 1.6 mm holes, plus enough running clearance to avoid heating the plastic.
hole_diameter = bit_size == 54 ? 1.9 : 1.8;
hole_offsets = style == "riley" ? [0, 10, 15] : [0, 6, 12];
guide_length = hole_offsets[len(hole_offsets) - 1] + 2 * edge_margin;

difference() {
    cube([guide_length, guide_width, guide_thickness]);

    for (offset = hole_offsets) {
        translate([edge_margin + offset, guide_width / 2, -0.1])
            cylinder(h = guide_thickness + 0.2, d = hole_diameter, $fn = 48);
    }
}
