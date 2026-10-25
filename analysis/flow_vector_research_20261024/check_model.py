"""Check the vector model against the project's 1-D Marshall model and
against direct numerical integration.

1. 1-D limit: r on x, K isotropic, u along x  ->  xbar^2 * S equals
   fit_4x4_pulse.finite_pulse_response (same zeta, tau, eps, dur).
2. Anisotropic kernel: compare against brute-force 2-D quadrature of the
   instantaneous anisotropic Gaussian (independent derivation).
"""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import fit_4x4_pulse as fit  # noqa: E402
import vecmodel as vm  # noqa: E402

t = np.linspace(1.5, 180, 600)
worst = 0.0
for k in (0.0016, 0.0025, 0.0034):
    for v_cm_h in (-4.0, 0.0, 10.0, 32.0):
        for eps in (-0.1, 0.0, 0.1):
            for side in (1, -1):
                xbar = 0.6
                tau = xbar**2 / k
                zeta = (v_cm_h / 3600.0) * xbar / k
                ref = fit.finite_pulse_response(t, zeta, tau, eps, side, 1.0, nodes=16)
                rx = side * xbar * (1 + side * eps)
                mine = xbar**2 * vm.line_response(t, rx, 0.0, k, k, 0.0, v_cm_h / 3600.0, 0.0, 1.0)
                worst = max(worst, float(np.max(np.abs(mine - ref)) / np.max(np.abs(ref))))
print(f"1-D limit: max relative difference vs finite_pulse_response = {worst:.2e}")

# Independent check: rotate an anisotropic Gaussian by hand.
kL, kT, th = 0.0030, 0.0015, np.deg2rad(25.0)
ux, uy = 20 / 3600 * np.cos(np.deg2rad(40)), 20 / 3600 * np.sin(np.deg2rad(40))
rx, ry = 0.3, 0.5
Kp = np.diag([kL, kT])
R = np.array([[np.cos(th), -np.sin(th)], [np.sin(th), np.cos(th)]])
K = R @ Kp @ R.T
Ki = np.linalg.inv(K)
s_nodes = np.linspace(0, 1, 2001)
def brute(tt):
    out = []
    for ti in tt:
        ss = s_nodes[s_nodes < ti]
        tau = ti - ss
        d = np.stack((rx - ux * tau, ry - uy * tau))
        q = np.einsum("in,ij,jn->n", d, Ki, d)
        f = np.exp(-q / (4 * tau)) / (tau * np.sqrt(np.linalg.det(K)))
        out.append(np.trapezoid(f, ss) if len(ss) > 1 else 0.0)
    return np.array(out)
tt = np.linspace(2, 150, 60)
b = brute(tt)
m = vm.line_response(tt, rx, ry, kL, kT, th, ux, uy, 1.0)
print(f"anisotropic kernel vs brute force: max rel diff = {np.max(np.abs(b - m)) / b.max():.2e}")
print(f"A_REF = {vm.A_REF:.4f} degC cm^2 (peak {vm.PEAK_REF} degC at {vm.S_REF} cm)")
