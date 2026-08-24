// ---------------------------------------------------------------------------
// Sap-flux field enclosure mounts  (QILIPSU 370x270x150 IP67 box)
//
// Two printed parts that bolt to the box's perforated mounting plate:
//   back_bracket + peg_cap  - carries the sap-flux logic-board enclosure by its
//                             two moulded zip-tie tabs (peg_cap x2).
//   battery_tray            - 90-degree L: cups the bottom of the LiFePO4 pack,
//                             back panel reaches up to the mounting plate.
//
// MOUNTING PLATE GRID (measured): 3.0 mm square holes on a 5.0 mm pitch in both
// directions (3 mm hole / 2 mm web).  M3 passes a 3 mm square hole easily
// (diagonal 4.24 mm).  Every screw pattern below is an integer multiple of the
// 5 mm pitch so all screws land at once; the holes are still slotted so the part
// can be nudged into phase during install.
//
// Both parts stand off the plate on ribs/bosses so screw heads and hardware
// already protruding from the plate face cannot hold the part off.
//
// Render one part at a time:
//   openscad -D 'part="back_bracket"' -o back_bracket.stl mounts.scad
//   openscad -D 'part="peg_cap"'      -o peg_cap.stl      mounts.scad
//   openscad -D 'part="battery_tray"' -o battery_tray.stl mounts.scad
//   openscad -D 'part="assembly"'     mounts.scad          (visual check only)
//
// All dimensions in mm.
// ---------------------------------------------------------------------------

part = "assembly";   // back_bracket | peg_cap | battery_tray | assembly

$fn = 64;
EPS = 0.01;

// ============================ shared / grid ================================
GRID_P    = 5.0;    // mounting-plate hole pitch, both axes
M3_CLR    = 3.4;    // M3 clearance
SLOT_TRAV = 6.6;    // extra travel along the slot (total slot length = M3_CLR + this)
STANDOFF  = 4.0;    // how far the part is held off the plate face (screw-head relief)

// M3 hex nut (DIN934) captured in the bracket
NUT_CC    = 6.6;    // across CORNERS for cylinder(d=,$fn=6); AF = 6.6*cos30 = 5.72
NUT_DEP   = 2.6;    // pocket depth (nut is 2.4 thick -> fully recessed)

// ==================== logic-board enclosure, back face =====================
// From enclosure/generate_enclosure.py (rev 7c).  The box back is FLAT and the
// two trapezoidal zip-tie tabs are flush with it, so back + tabs are coplanar.
BOX_W     = 106.0;  // outer, along the board's bx axis
BOX_H     = 71.0;   // outer, along by
TIE_SP    = 121.0;  // tie-hole centres  (= BOX_W + 2 * 7.5 mm offset)
TIE_D     = 8.0;    // tie-hole diameter
TAB_T     = 4.0;    // tab thickness
TAB_ROOT  = 40.0;   // tab width at the wall
TAB_TIP   = 24.0;   // tab width at its outer edge
TAB_OUT   = 15.0;   // how far the tab reaches past the wall
BOXNUT_X  = 46.0;   // +- : the box's own M3 nut pockets, box-centre coords
BOXNUT_Y  = 28.5;   // +-

// ============================ back bracket =================================
BRK_W     = 136.0;  // = BOX_W + 2*TAB_OUT, i.e. exactly the box+tabs footprint
BRK_H     = 66.0;
BRK_T     = 3.5;
BRK_R     = 4.0;    // corner radius

BRK_SCR_X = 57.5;   // +- ; 115 mm apart = 23 pitches. Outboard of the box body
BRK_SCR_Y = 25.0;   // +- ;  50 mm apart = 10 pitches. Clear of the tabs by 7.4 mm
BOSS_D    = 14.0;   // standoff boss on the back face, one per grid screw

// Cap that locates AND traps a zip-tie tab.  The locating spigot lives on the
// CAP, not on the bracket, so the bracket's front face stays dead flat and can
// be printed straight onto the bed with no supports.  The spigot is shorter
// than the tab, so the head lands on the TAB (not on the bracket) and clamps it.
CAP_D     = 13.0;   // > TIE_D, so it grips 2.5 mm of tab all round
CAP_T     = 3.0;
CAP_SPG_D = 7.6;    // slip fit in the 8.0 tie hole
CAP_SPG_H = 3.6;    // < TAB_T (4.0) -> head clamps the tab, spigot never bottoms out
CAP_CB_D  = 6.6;    // counterbore for an M3 pan head
CAP_CB_H  = 2.0;
// Screw: M3 x 8 pan head.  head seat z=8.5, nut spans z=0.2-2.6
// -> 2.1 mm of nut engaged and NOTHING protrudes past the bracket back face.

// ============================= battery tray ================================
BAT_X     = 94.5;   // across the plate
BAT_Y     = 65.5;   // plate face -> box front
BAT_Z     = 150.5;  // up
BAT_CLR   = 1.5;    // total slop added to each footprint dimension

TRAY_WALL = 3.0;
TRAY_FLR  = 3.0;
TRAY_LIP  = 30.0;   // cup wall height above the enclosure floor
BACK_T    = 5.0;    // outer skin that bolts to the plate
CAV       = 7.0;    // hollow between the two skins = relief for protruding screws
BACK_Z    = 80.0;   // how far the back panel reaches up the battery

TRAY_SCR_X = 25.0;  // +- ; 50 mm = 10 pitches
TRAY_SCR_Z = [27.0, 47.0, 67.0];   // 20 mm apart = 4 pitches
RAIL_W     = 20.0;  // vertical contact rail on the back, one per screw column

// ---- derived: y planes, measured from the mounting-plate face -------------
Y_RAIL0 = 0;                    Y_RAIL1 = STANDOFF;          // 0 - 4
Y_SKIN0 = Y_RAIL1;              Y_SKIN1 = Y_SKIN0 + BACK_T;  // 4 - 9
Y_CAV1  = Y_SKIN1 + CAV;                                     // 9 - 16
Y_CUPB1 = Y_CAV1 + TRAY_WALL;                                // 16 - 19  (cup back wall)
CUP_ID_X = BAT_X + BAT_CLR;
CUP_ID_Y = BAT_Y + BAT_CLR;
Y_CUP1  = Y_CUPB1 + CUP_ID_Y;                                // 19 - 86
Y_FRONT = Y_CUP1 + TRAY_WALL;                                // 86 - 89
TRAY_OD_X = CUP_ID_X + 2*TRAY_WALL;                          // 102

// ===========================================================================
// helpers
// ===========================================================================
module rrect(w, h, t, r) {          // rounded rectangular slab, centred in x/y
    linear_extrude(t)
        offset(r=r) offset(r=-r) square([w, h], center=true);
}

// slot along X, centred at origin, drilled through +Z
module slot_x(len, d, h) {
    hull() for (s = [-1, 1]) translate([s*(len-d)/2, 0, 0]) cylinder(d=d, h=h);
}

// slot along Z (used on the tray's vertical back panel; drilled through +Y)
module slot_z(len, d, h) {
    rotate([-90, 0, 0]) hull()
        for (s = [-1, 1]) translate([0, s*(len-d)/2, 0]) cylinder(d=d, h=h);
}

// ===========================================================================
// PART 1 - back_bracket
// Frame: X across the plate, Y up the plate, Z out from the plate face.
//        z=0 is the plate face; the standoff bosses live in -Z.
// Print: FRONT FACE DOWN (flat on the bed), bosses and nut pockets facing up.
//        No supports.
// ===========================================================================
module back_bracket() {
    difference() {
        union() {
            translate([0, 0, 0]) rrect(BRK_W, BRK_H, BRK_T, BRK_R);
            // standoff bosses (hold the plate off any hardware on the grid)
            for (sx = [-1, 1], sy = [-1, 1])
                translate([sx*BRK_SCR_X, sy*BRK_SCR_Y, -STANDOFF])
                    cylinder(d=BOSS_D, h=STANDOFF + EPS);
        }
        // grid screws: slotted in X so the part can be nudged into phase
        for (sx = [-1, 1], sy = [-1, 1])
            translate([sx*BRK_SCR_X, sy*BRK_SCR_Y, -STANDOFF - 1])
                slot_x(M3_CLR + SLOT_TRAV, M3_CLR, STANDOFF + BRK_T + 2);
        // retaining screw + captive nut under each zip-tie tab
        for (s = [-1, 1]) {
            translate([s*TIE_SP/2, 0, -1])
                cylinder(d=M3_CLR, h=BRK_T + 2);
            translate([s*TIE_SP/2, 0, -EPS])
                cylinder(d=NUT_CC, h=NUT_DEP + EPS, $fn=6);
        }
        // shallow relief over the box's own four M3 nut pockets, in case a
        // pressed nut sits a hair proud and would rock the box
        for (sx = [-1, 1], sy = [-1, 1])
            translate([sx*BOXNUT_X, sy*BOXNUT_Y, BRK_T - 0.8])
                cylinder(d=8.5, h=1.0);
    }
}

// ===========================================================================
// PART 2 - peg_cap  (print 2).  Spigot locates in a zip-tie tab; head clamps it.
// Print: HEAD DOWN on the bed, spigot up.  No supports.
// ===========================================================================
module peg_cap() {
    difference() {
        union() {
            cylinder(d=CAP_SPG_D, h=CAP_SPG_H);
            translate([0, 0, CAP_SPG_H - EPS])
                cylinder(d=CAP_D, h=CAP_T + EPS);
        }
        translate([0, 0, -1]) cylinder(d=M3_CLR, h=CAP_SPG_H + CAP_T + 2);
        translate([0, 0, CAP_SPG_H + CAP_T - CAP_CB_H])
            cylinder(d=CAP_CB_D, h=CAP_CB_H + 1);
    }
}

// ===========================================================================
// PART 3 - battery_tray
// Frame: X across the plate, Y out from the plate face, Z up from the
//        enclosure floor.  The tray floor rests on the enclosure floor; the
//        back panel only has to stop the pack tipping, not carry its weight.
// Print: floor on the bed.  Every wall is vertical -> zero supports.
// ===========================================================================
module battery_tray() {
    difference() {
        union() {
            // --- floor slab (runs back under the duct so the tray can't rock)
            translate([-TRAY_OD_X/2, Y_RAIL1, 0])
                cube([TRAY_OD_X, Y_FRONT - Y_RAIL1, TRAY_FLR]);
            // --- cup walls
            translate([-TRAY_OD_X/2, Y_CAV1, 0])
                difference() {
                    cube([TRAY_OD_X, Y_FRONT - Y_CAV1, TRAY_LIP]);
                    translate([TRAY_WALL, TRAY_WALL, TRAY_FLR])
                        cube([CUP_ID_X, CUP_ID_Y, TRAY_LIP]);
                }
            // --- back duct: outer skin + tall cup back wall + side webs.
            //     Closed box section -> stiff against the pack leaning on it.
            translate([-TRAY_OD_X/2, Y_SKIN0, 0])
                cube([TRAY_OD_X, BACK_T, BACK_Z]);                    // outer skin
            translate([-TRAY_OD_X/2, Y_CAV1, 0])
                cube([TRAY_OD_X, TRAY_WALL, BACK_Z]);                 // inner skin
            for (s = [-1, 1])                                          // side webs,
                translate([s > 0 ? TRAY_OD_X/2 - TRAY_WALL : -TRAY_OD_X/2,
                           Y_SKIN1, 0])                                // flush with the
                    cube([TRAY_WALL, CAV, BACK_Z]);                    // cup side walls
            // --- contact rails: the ONLY thing that touches the mounting plate
            for (s = [-1, 1])
                translate([s*TRAY_SCR_X - RAIL_W/2, Y_RAIL0, 0])
                    cube([RAIL_W, STANDOFF, BACK_Z]);
        }
        // --- grid screws: slotted vertically (the tray's height is fixed by the
        //     floor, so the slot must absorb the grid's phase)
        for (s = [-1, 1], z = TRAY_SCR_Z) {
            translate([s*TRAY_SCR_X, Y_RAIL0 - 1, z])
                slot_z(M3_CLR + SLOT_TRAV, M3_CLR, STANDOFF + BACK_T + 2);
            // driver + head access through the inner skin
            translate([s*TRAY_SCR_X, Y_CAV1 - 1, z])
                slot_z(M3_CLR + SLOT_TRAV + 5, 9.0, TRAY_WALL + 2);
        }
        // --- finger scallop in the front wall, to lift the pack out
        translate([0, Y_CUP1 - 1, TRAY_FLR + 12])
            rotate([-90, 0, 0]) hull() for (s = [-1, 1])
                translate([s*14, 0, 0]) cylinder(d=12, h=TRAY_WALL + 2);
        // --- drain holes
        for (sx = [-1, 1], y = [Y_CUPB1 + 18, Y_CUPB1 + 48])
            translate([sx*25, y, -1]) cylinder(d=6, h=TRAY_FLR + 2);
    }
}

// ===========================================================================
// assembly preview (not for export)
// ===========================================================================
module assembly() {
    // ---- plate frame: X across the plate, Y up the plate, Z out from its face.
    // mounting-plate stand-in
    color("Silver") translate([0, -55, -STANDOFF - 2]) cube([240, 300, 2], center=true);

    // logic-board enclosure, up top
    translate([0, 60, 0]) {
        color("SteelBlue") back_bracket();
        %translate([-BOX_W/2, -BOX_H/2, BRK_T]) cube([BOX_W, BOX_H, 19.6]);
        for (s = [-1, 1])
            translate([s*TIE_SP/2, 0, BRK_T + TAB_T - CAP_SPG_H]) color("Orange") peg_cap();
    }

    // battery tray, floor on the enclosure floor.  rotate then mirror maps the
    // tray frame (Y out, Z up) into the plate frame (Y up, Z out).
    translate([0, -170, 0]) {
        color("SeaGreen") mirror([0, 1, 0]) rotate([90, 0, 0]) battery_tray();
        %translate([-BAT_X/2, TRAY_FLR, Y_CUPB1]) cube([BAT_X, BAT_Z, BAT_Y]);
    }
}

if      (part == "back_bracket") back_bracket();
else if (part == "peg_cap")      peg_cap();
else if (part == "battery_tray") battery_tray();
else                             assembly();
