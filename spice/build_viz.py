#!/usr/bin/env python3
"""Parse ngspice wrdata output and generate interactive HTML visualization."""

import json
import numpy as np

# --- Parse ngspice wrdata (interleaved time-value pairs) ---
raw = np.loadtxt("heater_sim.dat")
# wrdata format: each row has [t1 v1 t2 v2 ... t11 v11] for 11 signals
# All t values are identical per row, so just grab the first one
n_signals = 11
time = raw[:, 0]  # first time column

signal_names = [
    "pa8", "gate_node", "heater_lo", "pnp_coll", "arm_node",
    "rstn", "pgood", "driver_gate", "ctrl_gate", "pnp_base", "latch_base"
]
signals = {}
for i, name in enumerate(signal_names):
    signals[name] = raw[:, 2 * i + 1]  # value columns at indices 1,3,5,...

# Compute heater current: I = (V_fused - V_heater_lo) / R_heater
# V_fused ≈ 24V (ideal supply through 0.05Ω fuse)
heater_current = (24.0 - signals["heater_lo"]) / 5.0
# Clip negative values (flyback transients)
heater_current = np.clip(heater_current, 0, None)

# --- Downsample to ~6000 points for fast rendering ---
# But keep all points near transitions (where derivatives are large)
target_points = 6000
if len(time) > target_points:
    # Always keep first and last
    # Use stride but also keep points where any signal changes fast
    stride = max(1, len(time) // target_points)
    uniform = set(range(0, len(time), stride))
    uniform.add(len(time) - 1)

    # Find transition points (large derivatives in key signals)
    for sig_name in ["pa8", "gate_node", "pnp_coll", "rstn", "arm_node"]:
        sig = signals[sig_name]
        deriv = np.abs(np.diff(sig))
        threshold = np.percentile(deriv[deriv > 0], 95) if np.any(deriv > 0) else 1
        transition_idx = np.where(deriv > threshold * 0.1)[0]
        # Keep points around each transition
        for idx in transition_idx:
            for offset in range(-2, 3):
                if 0 <= idx + offset < len(time):
                    uniform.add(idx + offset)

    keep = sorted(uniform)
    time = time[keep]
    for name in signal_names:
        signals[name] = signals[name][keep]
    heater_current = heater_current[keep]

# Convert time to milliseconds for the boot region, seconds for full view
time_s = time.tolist()

# --- Build plotly traces ---
def make_trace(y_data, name, color, row):
    return {
        "x": time_s,
        "y": y_data.tolist(),
        "name": name,
        "type": "scattergl",
        "mode": "lines",
        "line": {"color": color, "width": 1.5},
        "xaxis": "x",
        "yaxis": f"y{row}" if row > 1 else "y",
    }

traces = [
    # Row 1: MCU command
    make_trace(signals["pa8"], "PA8 (MCU)", "#2196F3", 1),
    # Row 2: Gate voltage
    make_trace(signals["gate_node"], "Gate Node (Q1)", "#FF9800", 2),
    # Row 3: Heater current
    make_trace(heater_current, "Heater Current", "#f44336", 3),
    # Row 4: Latch signals
    make_trace(signals["pnp_coll"], "PNP Collector (Q4 gate)", "#9C27B0", 4),
    make_trace(signals["rstn"], "RSTn", "#607D8B", 4),
    # Row 5: Arming
    make_trace(signals["arm_node"], "Arm Node (RC)", "#4CAF50", 5),
    make_trace(signals["pgood"], "PGOOD", "#00BCD4", 5),
    # Row 6: Internal nodes
    make_trace(signals["driver_gate"], "Driver Gate (Q7)", "#795548", 6),
    make_trace(signals["pnp_base"], "PNP Base (Q2)", "#E91E63", 6),
    make_trace(signals["latch_base"], "Latch Base (Q3)", "#FF5722", 6),
]

# --- Layout with stacked subplots ---
row_labels = [
    ("MCU Command", "V"),
    ("Q1 Gate", "V"),
    ("Heater Current", "A"),
    ("Latch / Reset", "V"),
    ("PGOOD Arming", "V"),
    ("Internal Nodes", "V"),
]
n_rows = len(row_labels)
gap = 0.03
plot_height = (1.0 - gap * (n_rows - 1)) / n_rows

layout = {
    "title": {"text": "saptest1 Heater Section — SPICE Simulation", "font": {"size": 18}},
    "height": 900,
    "margin": {"t": 60, "b": 50, "l": 70, "r": 30},
    "showlegend": True,
    "legend": {"orientation": "h", "y": -0.05, "font": {"size": 10}},
    "hovermode": "x unified",
    "xaxis": {
        "title": "Time (s)",
        "domain": [0, 1],
        "rangeslider": {"visible": True, "thickness": 0.05},
    },
    "plot_bgcolor": "#1a1a2e",
    "paper_bgcolor": "#16213e",
    "font": {"color": "#e0e0e0"},
}

# Build y-axes from top to bottom
for i, (label, unit) in enumerate(row_labels):
    row_num = i + 1
    y_start = 1.0 - (i + 1) * plot_height - i * gap
    y_end = 1.0 - i * plot_height - i * gap
    axis_key = f"yaxis{row_num}" if row_num > 1 else "yaxis"
    layout[axis_key] = {
        "title": {"text": f"{label} ({unit})", "font": {"size": 10}},
        "domain": [max(0, y_start), y_end],
        "gridcolor": "#2a2a4a",
        "zerolinecolor": "#3a3a5a",
    }
    # All rows share the same x-axis
    if row_num > 1:
        layout[f"xaxis{row_num}"] = {"visible": False, "anchor": f"y{row_num}", "matches": "x"}
        # Update traces to use correct xaxis
        for trace in traces:
            if trace["yaxis"] == f"y{row_num}":
                trace["xaxis"] = f"x{row_num}"

# Add annotation boxes for scenarios
annotations = [
    {"x": 0.035, "y": 1.02, "text": "BOOT<br>(POR pulse)", "showarrow": False,
     "xref": "x", "yref": "paper", "font": {"size": 9, "color": "#aaa"}},
    {"x": 0.6, "y": 1.02, "text": "ARM<br>(RC delay)", "showarrow": False,
     "xref": "x", "yref": "paper", "font": {"size": 9, "color": "#aaa"}},
    {"x": 3.0, "y": 1.02, "text": "HEATER #1", "showarrow": False,
     "xref": "x", "yref": "paper", "font": {"size": 9, "color": "#4CAF50"}},
    {"x": 7.0, "y": 1.02, "text": "HEATER #2", "showarrow": False,
     "xref": "x", "yref": "paper", "font": {"size": 9, "color": "#4CAF50"}},
    {"x": 8.05, "y": 1.02, "text": "FAULT", "showarrow": False,
     "xref": "x", "yref": "paper", "font": {"size": 9, "color": "#f44336"}},
    {"x": 10.0, "y": 1.02, "text": "RETRY<br>(blocked)", "showarrow": False,
     "xref": "x", "yref": "paper", "font": {"size": 9, "color": "#f44336"}},
]
layout["annotations"] = annotations

# Add shapes for scenario regions
shapes = [
    # Boot region
    {"type": "rect", "x0": 0, "x1": 0.1, "y0": 0, "y1": 1,
     "xref": "x", "yref": "paper", "fillcolor": "#FFD700", "opacity": 0.05, "line": {"width": 0}},
    # Fault region
    {"type": "rect", "x0": 8, "x1": 8.1, "y0": 0, "y1": 1,
     "xref": "x", "yref": "paper", "fillcolor": "#f44336", "opacity": 0.1, "line": {"width": 0}},
    # Blocked retry region
    {"type": "rect", "x0": 9, "x1": 11, "y0": 0, "y1": 1,
     "xref": "x", "yref": "paper", "fillcolor": "#f44336", "opacity": 0.05, "line": {"width": 0}},
]
layout["shapes"] = shapes

# --- Generate HTML ---
html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>saptest1 Heater SPICE Simulation</title>
<script src="https://cdn.plot.ly/plotly-2.35.0.min.js"></script>
<style>
  body {{
    margin: 0; padding: 20px;
    background: #16213e; color: #e0e0e0;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
  }}
  h1 {{ text-align: center; margin: 0 0 5px; font-size: 16px; color: #aaa; }}
  p.info {{ text-align: center; font-size: 12px; color: #666; margin: 0 0 10px; }}
  #plot {{ width: 100%; }}
</style>
</head>
<body>
<h1>saptest1 Heater Section &mdash; ngspice Transient Simulation</h1>
<p class="info">Drag the range slider to zoom. Click legend entries to toggle signals. Hover for values.</p>
<div id="plot"></div>
<script>
const traces = {json.dumps(traces)};
const layout = {json.dumps(layout)};
Plotly.newPlot('plot', traces, layout, {{responsive: true}});
</script>
</body>
</html>
"""

with open("heater_sim.html", "w") as f:
    f.write(html)

print(f"Wrote heater_sim.html ({len(time)} data points)")
print(f"Time range: {time[0]:.6f}s to {time[-1]:.6f}s")
print(f"Key values at t=0: gate_node={signals['gate_node'][0]:.4f}V, heater_lo={signals['heater_lo'][0]:.4f}V")
# Find approximate values at t=3s (during heater pulse)
idx_3s = np.searchsorted(time, 3.0)
if idx_3s < len(time):
    print(f"Key values at t≈3s: gate_node={signals['gate_node'][idx_3s]:.4f}V, heater_lo={signals['heater_lo'][idx_3s]:.4f}V, I_heater={heater_current[idx_3s]:.4f}A")
# Find values at t=9.5s (during blocked retry)
idx_9s = np.searchsorted(time, 9.5)
if idx_9s < len(time):
    print(f"Key values at t≈9.5s: gate_node={signals['gate_node'][idx_9s]:.4f}V, pnp_coll={signals['pnp_coll'][idx_9s]:.4f}V (latch {'HOLDING' if signals['pnp_coll'][idx_9s] > 1.0 else 'NOT holding'})")
