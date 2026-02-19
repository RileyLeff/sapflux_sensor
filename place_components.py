#!/usr/bin/env python3
"""
PCB component placement script for saptest1 sap flux sensor adapter board.
Reads the atopile-generated KiCad PCB, places components into functional zones,
adds 4-layer stackup, board outline, mounting holes, fiducials, and zone fills.
"""

import re
import uuid as uuid_mod

PCB_PATH = "elec/layouts/default/default.kicad_pcb"

def gen_uuid():
    return str(uuid_mod.uuid4())

# ============================================================================
# Component placement map: atopile_address -> (x, y, rotation)
# Board: 90mm x 62mm, top-left at (100, 50)
# ============================================================================

PLACEMENTS = {
    # === CONNECTORS ===
    # CN1: M8 connector, left edge, body extends left off board
    # At 270deg rotation, body projects left. Place ~6mm in from left edge for pads.
    "connectors.j1": (106, 81, 270),
    # CN2: 10-pin header, bottom edge, 0deg (pins horizontal)
    "connectors.j2": (152, 109, 0),
    # C24: Shield cap for CN1
    "connectors.c5": (103, 76, 90),

    # === INPUT PROTECTION (near CN1, left side) ===
    # Signal flow: CN1 pin1 -> U1(PTC) -> D2(TVS to GND) -> D1(Schottky) -> protected 24V
    "input_protection.f1": (112, 86, 0),      # U1 PTC F2920
    "input_protection.d2_tvs": (112, 91, 0),   # D2 TVS SMAJ33A
    "input_protection.d1_schottky": (112, 96, 180),  # D1 Schottky SS36

    # === BUCK CONVERTER (upper-left) ===
    # Tight SW loop: C1/C4 -> U2 VIN -> SW -> L1 -> VOUT -> C2
    "buck.u_buck": (109, 58, 0),        # U2 LM5165 VSON-10 (3x3mm)
    "buck.c_vin": (105, 56, 0),         # C4 100nF VIN HF decoupling - tight to VIN
    "buck.c1": (105, 60, 0),            # C1 4.7uF 1206 input cap
    "buck.l1": (114, 58, 0),            # L1 68uH inductor (4x4mm) - adjacent to SW
    "buck.c2": (114, 63, 0),            # C2 22uF 0805 output cap
    "buck.c6": (110, 63, 0),            # C3 1uF VCC decoupling
    "buck.r_ilim": (106, 63, 90),       # R1 56k ILIM

    # === MCU (center, 180deg rotation) ===
    "mcu.package": (137, 72, 180),       # U3 STM32 LQFP-48 (8.5mm sq)
    # Decoupling caps - placed near VDD pins at each edge
    # At 180deg: top edge has pins 1-12 (VDD pins 4-6), bottom has 25-36
    # left has 13-24, right has 37-48
    "mcu.c_mcu1": (134, 67, 0),         # C5 100nF - near VDD pins top-left
    "mcu.c_mcu2": (140, 67, 0),         # C6 100nF - near VDD pins top-right
    "mcu.c_mcu3": (134, 77, 0),         # C7 100nF - near bottom edge
    "mcu.c_mcu4": (140, 77, 0),         # C8 100nF - near bottom edge
    "mcu.c_nrst": (141, 67, 0),         # C9 100nF NRST cap - near NRST pin (top edge at 180deg)
    # VDDA filter: L2 (ferrite) + C10 (1uF) on right side near VDDA pin
    "mcu.fb_vdda": (143, 70, 0),        # L2 ferrite bead 0603
    "mcu.c_vdda": (143, 72, 0),         # C10 1uF VDDA bulk
    "mcu.r_boot0": (135, 77, 90),       # R2 10k BOOT0 pull-down - near PA14 (bottom at 180deg)

    # === RS-485 (left, between CN1 and MCU) ===
    "rs485.d_rs485": (106, 74, 0),       # D5 SM712 SOT-23 - nearest to CN1
    "rs485.transceiver.package": (116, 74, 0),  # U6 SP3485 SOIC-8
    "rs485.transceiver.vcc_decoupling": (113, 71, 0),  # C22 0402 VCC bypass
    "rs485.r_de_pd": (121, 72, 90),      # R17 100k DE pull-down
    "rs485.r_re_pd": (123, 72, 90),      # R18 100k ~RE pull-down
    "rs485.r_term": (108, 79, 0),        # R19 120ohm termination
    "rs485.c_term": (108, 81, 0),        # C21 100nF termination cap

    # === HEATER DRIVER (lower-center-left) ===
    # Power path: 24V -> CN2 pin1 -> heater -> CN2 pin2 -> Q1 -> GND
    "heater.q1": (130, 103, 0),          # Q1 MOSFET SOT-23 - near CN2 heater pins
    "heater.d4": (126, 106, 180),        # D6 flyback SMA - across heater terminals
    "heater.c4": (122, 103, 0),          # U7 100uF bulk cap - near Q1
    # Gate control
    "heater.r_gate": (133, 99, 90),      # R24 1k gate series R
    "heater.r4": (135, 99, 90),          # R21 10k gate pull-down
    # TPL5010 watchdog
    "heater.u_wdt": (120, 93, 0),        # U8 TPL5010 SOT-23-6
    "heater.c_wd": (117, 91, 0),         # C23 100nF TPL5010 bypass
    "heater.r_delay": (123, 91, 0),      # R22 11k timeout resistor
    # SCR latch circuit
    "heater.q2_inv": (125, 95, 0),       # Q2 PNP inverter SOT-23
    "heater.q_wd": (129, 95, 0),         # Q4 2N7002 gate clamp SOT-23
    "heater.q_latch": (125, 99, 0),      # Q3 NPN latch SOT-23
    "heater.q_arm": (121, 99, 0),        # Q5 2N7002 arm FET SOT-23
    "heater.d_iso": (123, 97, 0),        # D7 1N4148WS SOD-323
    # Heater resistors
    "heater.r_wd_pd": (131, 93, 90),     # R20 10k Q_WD gate pull-down
    "heater.r_wd_pu": (117, 95, 90),     # R23 100k RSTn pull-up
    "heater.r_base": (124, 93, 90),      # R26 10k PNP base R
    "heater.r_fb": (127, 97, 90),        # R27 100k feedback R
    "heater.r_latch_pd": (123, 101, 0),  # R25 1M latch base pull-down
    "heater.r_pnp_base": (127, 93, 0),   # R29 100k PNP base-emitter pull-up
    "heater.r_arm": (119, 99, 90),       # R34 1M arm delay R
    "heater.c_arm": (119, 101, 90),      # C28 1uF arm delay cap
    "heater.r_done_pd": (120, 97, 0),    # R28 100k DONE pull-down

    # === ADC FRONT-END (right side) ===
    # ADC ICs (upper-right)
    "adc.adc1.package": (165, 58, 0),    # U4 ADS1115 #1 MSOP-10
    "adc.adc2.package": (177, 58, 0),    # U5 ADS1115 #2 MSOP-10
    # ADC1 support (near U4)
    "adc.adc1.decoupling_cap": (163, 55, 0),  # C11 0402 bypass
    "adc.adc1.sda_pullup": (162, 61, 0),      # R5 0402 SDA pull-up
    "adc.adc1.scl_pullup": (164, 61, 0),      # R4 0402 SCL pull-up
    "adc.adc1.alert_pullup": (168, 55, 0),    # R3 0402 ALERT pull-up
    # ADC2 support (near U5)
    "adc.adc2.decoupling_cap": (175, 55, 0),  # C12 0402 bypass
    "adc.adc2.sda_pullup": (174, 61, 0),      # R8 0402 SDA pull-up
    "adc.adc2.scl_pullup": (176, 61, 0),      # R7 0402 SCL pull-up
    "adc.adc2.alert_pullup": (180, 55, 0),    # R6 0402 ALERT pull-up

    # TVS arrays (between CN2 and ADC ICs)
    "adc.d_j2a": (165, 71, 0),           # D3 PRTR5V0U4D - excitation + ch 1-3
    "adc.d_j2b": (177, 71, 0),           # D4 PRTR5V0U4D - ch 4-6
    # TVS local decoupling
    "adc.c_esd_a": (162, 69, 0),         # C26 100nF D3 VCC decoupling
    "adc.c_esd_b": (174, 69, 0),         # C27 100nF D4 VCC decoupling

    # Excitation filter
    "adc.r_exc": (158, 75, 90),          # R16 560ohm excitation series R
    "adc.c_exc": (158, 65, 0),           # C20 1uF excitation filter cap

    # Input protection: series R column (between CN2 and TVS)
    "adc.r_ain_exc": (161, 75, 90),      # R9/R_AIN_EXC 1k excitation sense
    "adc.r_ain1": (164, 75, 90),         # R10 1k thermistor 1
    "adc.r_ain2": (167, 75, 90),         # R11 1k thermistor 2
    "adc.r_ain3": (170, 75, 90),         # R12 1k thermistor 3
    "adc.r_ain4": (173, 75, 90),         # R13 1k thermistor 4
    "adc.r_ain5": (176, 75, 90),         # R14 1k thermistor 5
    "adc.r_ain6": (179, 75, 90),         # R15 1k thermistor 6

    # Filter cap column (after series R, before ADC)
    "adc.c_ain_exc": (161, 79, 90),      # C19 100nF excitation filter
    "adc.c_ain1": (164, 79, 90),         # C13 100nF thermistor 1
    "adc.c_ain2": (167, 79, 90),         # C14 100nF thermistor 2
    "adc.c_ain3": (170, 79, 90),         # C15 100nF thermistor 3
    "adc.c_ain4": (173, 79, 90),         # C16 100nF thermistor 4
    "adc.c_ain5": (176, 79, 90),         # C17 100nF thermistor 5
    "adc.c_ain6": (179, 79, 90),         # C18 100nF thermistor 6

    # === EEPROM + LED (center-right, between MCU and ADCs) ===
    "eeprom": (152, 70, 0),              # U9 EEPROM SOIC-8
    "c_eeprom": (149, 68, 0),            # C25 100nF bypass
    "led.led": (147, 56, 0),             # U10 RGB LED
    "led.r_r": (144, 55, 90),            # R32 680ohm red
    "led.r_g": (146, 55, 90),            # R31 200ohm green
    "led.r_b": (148, 55, 90),            # R30 220ohm blue

    # === TOP-LEVEL ===
    "r_pgood": (123, 65, 0),             # R33 10k PGOOD pull-up - between buck and MCU
}


def update_layers(content):
    """Add inner copper layers for 4-layer stackup."""
    # Add In1.Cu and In2.Cu after B.Cu line
    content = content.replace(
        '\t\t(31 "B.Cu" signal)',
        '\t\t(1 "In1.Cu" signal "GND")\n\t\t(2 "In2.Cu" signal "3V3")\n\t\t(31 "B.Cu" signal)'
    )
    return content


def update_stackup(content):
    """Update stackup to 4-layer with proper dielectrics."""
    old_stackup = '''		(layer "F.Cu"
			(type "copper")
			(thickness 0.035)
		)
		(layer "dielectric 1"
			(type "core")
			(thickness 1.51)
			(material "FR4")
			(epsilon_r 4.5)
			(loss_tangent 0.02)
		)
		(layer "B.Cu"
			(type "copper")
			(thickness 0.035)
		)'''

    new_stackup = '''		(layer "F.Cu"
			(type "copper")
			(thickness 0.035)
		)
		(layer "dielectric 1"
			(type "prepreg")
			(thickness 0.21)
			(material "FR4")
			(epsilon_r 4.5)
			(loss_tangent 0.02)
		)
		(layer "In1.Cu"
			(type "copper")
			(thickness 0.0175)
		)
		(layer "dielectric 2"
			(type "core")
			(thickness 1.065)
			(material "FR4")
			(epsilon_r 4.5)
			(loss_tangent 0.02)
		)
		(layer "In2.Cu"
			(type "copper")
			(thickness 0.0175)
		)
		(layer "dielectric 3"
			(type "prepreg")
			(thickness 0.21)
			(material "FR4")
			(epsilon_r 4.5)
			(loss_tangent 0.02)
		)
		(layer "B.Cu"
			(type "copper")
			(thickness 0.035)
		)'''

    content = content.replace(old_stackup, new_stackup)
    return content


def place_components(content):
    """Update component positions based on atopile_address mapping."""
    placed = set()
    not_found = []

    for addr, (x, y, rot) in PLACEMENTS.items():
        # Find the footprint block containing this atopile_address
        # Pattern: "atopile_address" "<addr>"
        pattern = f'"atopile_address" "{addr}"'
        idx = content.find(pattern)
        if idx == -1:
            not_found.append(addr)
            continue

        # Find the start of this footprint block (search backwards for "(footprint")
        fp_start = content.rfind("(footprint", 0, idx)
        if fp_start == -1:
            not_found.append(addr)
            continue

        # Find the (at X Y rotation) within this footprint block
        # Search from fp_start to some reasonable distance
        fp_section = content[fp_start:fp_start + 2000]

        # Match (at X Y) or (at X Y rotation)
        at_match = re.search(r'\(at (-?[\d.]+) (-?[\d.]+)( -?[\d.]+)?\)', fp_section)
        if at_match:
            old_at = at_match.group(0)
            if rot == 0:
                new_at = f"(at {x} {y})"
            else:
                new_at = f"(at {x} {y} {rot})"

            # Replace in the original content (only first occurrence from fp_start)
            old_pos = content.find(old_at, fp_start)
            if old_pos != -1 and old_pos < fp_start + 2000:
                content = content[:old_pos] + new_at + content[old_pos + len(old_at):]
                placed.add(addr)
            else:
                not_found.append(addr)
        else:
            not_found.append(addr)

    print(f"Placed {len(placed)} components")
    if not_found:
        print(f"NOT FOUND ({len(not_found)}): {not_found}")
    return content


def remove_traces(content):
    """Remove all auto-generated segments and vias."""
    # Remove segment lines
    content = re.sub(r'\t\(segment\n(?:\t\t[^\n]+\n)+\t\)\n', '', content)
    # Also try single-line segments
    content = re.sub(r'\t\(segment [^\n]+\n', '', content)
    # Remove vias
    content = re.sub(r'\t\(via\n(?:\t\t[^\n]+\n)+\t\)\n', '', content)
    content = re.sub(r'\t\(via [^\n]+\n', '', content)
    return content


def add_board_outline(content):
    """Add Edge.Cuts board outline rectangle."""
    outline = f'''
	(gr_rect
		(start 100 50)
		(end 190 112)
		(stroke
			(width 0.05)
			(type default)
		)
		(fill none)
		(layer "Edge.Cuts")
		(uuid "{gen_uuid()}")
	)
'''
    # Insert before the closing paren of the file
    content = content.rstrip()
    if content.endswith(')'):
        content = content[:-1] + outline + ')\n'
    return content


def add_mounting_holes(content):
    """Add 4x M3 mounting holes."""
    positions = [(104, 54), (186, 54), (104, 108), (186, 108)]
    holes = ""
    for i, (x, y) in enumerate(positions):
        holes += f'''
	(footprint "MountingHole:MountingHole_3.2mm_M3_Pad"
		(layer "F.Cu")
		(uuid "{gen_uuid()}")
		(at {x} {y})
		(property "Reference" "MH{i+1}"
			(at 0 -3.5)
			(layer "F.SilkS")
			(uuid "{gen_uuid()}")
			(effects
				(font
					(size 1 1)
					(thickness 0.15)
				)
			)
		)
		(property "Value" "MountingHole"
			(at 0 3.5)
			(layer "F.Fab")
			(uuid "{gen_uuid()}")
			(effects
				(font
					(size 1 1)
					(thickness 0.15)
				)
			)
		)
		(pad "1" thru_hole circle
			(at 0 0)
			(size 6 6)
			(drill 3.2)
			(layers "*.Cu" "*.Mask")
			(net 73 "lv")
			(uuid "{gen_uuid()}")
		)
	)
'''
    content = content.rstrip()
    if content.endswith(')'):
        content = content[:-1] + holes + ')\n'
    return content


def add_fiducials(content):
    """Add 3x fiducial markers."""
    positions = [(104, 52), (188, 52), (104, 110)]
    fids = ""
    for i, (x, y) in enumerate(positions):
        fids += f'''
	(footprint "Fiducial:Fiducial_1mm_Mask2mm"
		(layer "F.Cu")
		(uuid "{gen_uuid()}")
		(at {x} {y})
		(property "Reference" "FID{i+1}"
			(at 0 -2)
			(layer "F.SilkS")
			(uuid "{gen_uuid()}")
			(effects
				(font
					(size 0.8 0.8)
					(thickness 0.1)
				)
			)
		)
		(property "Value" "Fiducial"
			(at 0 2)
			(layer "F.Fab")
			(uuid "{gen_uuid()}")
			(effects
				(font
					(size 0.8 0.8)
					(thickness 0.1)
				)
			)
		)
		(pad "1" smd circle
			(at 0 0)
			(size 1 1)
			(layers "F.Cu" "F.Mask")
			(solder_mask_margin 0.5)
			(uuid "{gen_uuid()}")
		)
	)
'''
    content = content.rstrip()
    if content.endswith(')'):
        content = content[:-1] + fids + ')\n'
    return content


def add_zones(content):
    """Add GND and 3.3V copper zone fills."""
    # GND zone on In1.Cu - covers entire board
    gnd_zone = f'''
	(zone
		(net 73)
		(net_name "lv")
		(layer "In1.Cu")
		(uuid "{gen_uuid()}")
		(hatch edge 0.5)
		(connect_pads
			(clearance 0.3)
		)
		(min_thickness 0.2)
		(fill yes
			(thermal_gap 0.3)
			(thermal_bridge_width 0.3)
		)
		(polygon
			(pts
				(xy 100 50) (xy 190 50) (xy 190 112) (xy 100 112)
			)
		)
	)
'''
    # 3.3V zone on In2.Cu - covers MCU + ADC + digital area (right 2/3 of board)
    v33_zone = f'''
	(zone
		(net 74)
		(net_name "power_out-hv")
		(layer "In2.Cu")
		(uuid "{gen_uuid()}")
		(hatch edge 0.5)
		(connect_pads
			(clearance 0.3)
		)
		(min_thickness 0.2)
		(fill yes
			(thermal_gap 0.3)
			(thermal_bridge_width 0.3)
		)
		(polygon
			(pts
				(xy 120 50) (xy 190 50) (xy 190 100) (xy 120 100)
			)
		)
	)
'''
    # GND zone on B.Cu - entire board (secondary ground fill)
    gnd_bcu = f'''
	(zone
		(net 73)
		(net_name "lv")
		(layer "B.Cu")
		(uuid "{gen_uuid()}")
		(hatch edge 0.5)
		(connect_pads
			(clearance 0.3)
		)
		(min_thickness 0.2)
		(fill yes
			(thermal_gap 0.3)
			(thermal_bridge_width 0.3)
		)
		(polygon
			(pts
				(xy 100 50) (xy 190 50) (xy 190 112) (xy 100 112)
			)
		)
	)
'''

    content = content.rstrip()
    if content.endswith(')'):
        content = content[:-1] + gnd_zone + v33_zone + gnd_bcu + ')\n'
    return content


def main():
    with open(PCB_PATH, 'r') as f:
        content = f.read()

    print(f"Read {len(content)} chars, {content.count(chr(10))} lines")

    # Step 1: Update layers for 4-layer
    content = update_layers(content)
    print("Updated layers to 4-layer")

    # Step 2: Update stackup
    content = update_stackup(content)
    print("Updated stackup")

    # Step 3: Remove existing auto-traces
    before = content.count('(segment')
    content = remove_traces(content)
    after = content.count('(segment')
    print(f"Removed traces: {before} -> {after} segments remaining")

    # Step 4: Place components
    content = place_components(content)

    # Step 5: Add board outline
    content = add_board_outline(content)
    print("Added board outline")

    # Step 6: Add mounting holes
    content = add_mounting_holes(content)
    print("Added mounting holes")

    # Step 7: Add fiducials
    content = add_fiducials(content)
    print("Added fiducials")

    # Step 8: Add zones
    content = add_zones(content)
    print("Added zone fills")

    with open(PCB_PATH, 'w') as f:
        f.write(content)

    print(f"\nWrote {len(content)} chars to {PCB_PATH}")
    print("Done! Open in KiCad to verify.")


if __name__ == "__main__":
    main()
