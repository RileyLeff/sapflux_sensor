"""What today's 1-D whole-curve fit reports when the probe is rotated.

Noise-free curves from the anisotropic vector model for the East30 layout
(thermistors at +-0.6 cm on the probe's x axis), with the grain AND the flow
rotated by angle theta from x (a crooked install, or spiral grain).  The
curves go through the project's own estimator, fit_4x4_pulse.fit_pair, and
the reported velocity is compared with the closed form derived in README
section 2.3:

    V_app / U = cos(theta) / (cos^2 theta + R sin^2 theta),   R = kL / kT
    k_app     = kL / (cos^2 theta + R sin^2 theta)

Output: results/misalign_1d.json, fig_misalign_1d.png
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "compression_research_20261024"))
import fit_4x4_pulse as fit  # noqa: E402
import vecmodel as vm  # noqa: E402
from common import fit_args  # noqa: E402

t = np.arange(-10.0, 181.0, 1 / 25.0)
args = fit_args()
kL = 0.0025
rows = []
for R in (1.0, 1.5, 1.85, 2.5):
    kT = kL / R
    for U in (5.0, 15.0, 30.0):
        for th_deg in (0, 2, 5, 10, 15, 20, 30, 45):
            th = np.deg2rad(th_deg)
            ux, uy = U * np.cos(th) / 3600, U * np.sin(th) / 3600
            fr = pd.DataFrame({"time_s": t})
            for ch, x in ((1, 0.6), (2, -0.6)):
                y = np.zeros_like(t)
                m = t > 0
                y[m] = vm.A_REF * vm.line_response(t[m], x, 0.0, kL, kT, th, ux, uy, 1.0)
                fr[f"t{ch}_rise_c"] = y
            res, _ = fit.fit_pair(fr, "x", 1, 2, 1.0, args)
            d = np.cos(th) ** 2 + R * np.sin(th) ** 2
            rows.append(dict(
                R=R, U=U, theta_deg=th_deg,
                v_fit=res.nominal_velocity_cm_hr, k_fit=res.nominal_k_cm2_s,
                v_closed=U * np.cos(th) / d, k_closed=kL / d,
                eps_fit=res.epsilon, trace_rmse_mK=1e3 * res.trace_rmse_c,
            ))
df = pd.DataFrame(rows)
df["v_ratio"] = df.v_fit / df.U
df["closed_ratio"] = df.v_closed / df.U
(HERE / "results").mkdir(exist_ok=True)
df.to_json(HERE / "results/misalign_1d.json", orient="records", indent=1)
pd.set_option("display.width", 160)
print(df[df.U == 15.0][["R", "theta_deg", "v_fit", "v_closed", "v_ratio", "closed_ratio", "k_fit", "k_closed", "trace_rmse_mK"]].round(4).to_string(index=False))
print("max |fit - closed| / U over all cases:", float(np.max(np.abs(df.v_fit - df.v_closed) / df.U)))

fig, ax = plt.subplots(1, 2, figsize=(11, 4.2))
th = np.linspace(0, 45, 200)
for R, c in zip((1.0, 1.5, 1.85, 2.5), ("#444", "#3B82F6", "#E69F00", "#B91C1C")):
    r = np.deg2rad(th)
    ax[0].plot(th, np.cos(r) / (np.cos(r) ** 2 + R * np.sin(r) ** 2), color=c, label=f"R = kL/kT = {R}")
    sub = df[(df.R == R) & (df.U == 15.0)]
    ax[0].plot(sub.theta_deg, sub.v_ratio, "o", color=c, ms=4)
    ax[1].plot(th, 1 / (np.cos(r) ** 2 + R * np.sin(r) ** 2), color=c)
    ax[1].plot(sub.theta_deg, sub.k_fit / kL, "o", color=c, ms=4)
ax[0].set(xlabel="probe rotation from grain/flow (deg)", ylabel="reported V / true speed",
          title="Today's 1-D fit under rotation (dots: fit_pair, lines: closed form)")
ax[1].set(xlabel="probe rotation (deg)", ylabel="reported k / kL", title="Apparent diffusivity")
ax[0].legend(fontsize=8)
for a in ax:
    a.grid(alpha=0.3)
fig.tight_layout()
fig.savefig(HERE / "fig_misalign_1d.png", dpi=140)
