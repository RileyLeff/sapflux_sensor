"""Can a needle-parallel (radial) flow component be measured with a line heater?

With an infinitely long heater along z, the solution is invariant along z,
so u_z drops out exactly: radial flow is invisible.  A real heater has ends.
For a heater occupying z in [z1, z2] and a medium with radial diffusivity
kR, the 3-D solution is the 2-D one times

    F(z, tau) = 1/2 [ erf((z - z1 - uz tau) / (2 sqrt(kR tau)))
                    - erf((z - z2 - uz tau) / (2 sqrt(kR tau))) ]

inside the source-time integral.  u_z only enters through the erf edges,
i.e. only thermistors within a diffusion length (2 sqrt(kR t) ~ 1 cm at
t = 100 s) of a heater end see it, and there it is confounded with the end
positions z1, z2 (where the heating element starts/stops inside the needle,
where the bark/cambium boundary is) and with the needle's own axial heat
conduction (not modelled here, which makes this an optimistic bound).

Geometry (an assumption: East30 internal dimensions are not published;
it mimics common 35 mm three-depth probes): needles 3.5 cm long, heater
element along z in [0.0, 3.5] cm (z = depth below the cambium), thermistors
at depths 0.5, 1.75, 3.0 cm on needles at x = +-0.6 cm.  A second design
puts the deepest thermistor 0.3 cm beyond a shortened heater (z2 = 2.7).

CRB on u_z with nuisance: ux, kL, kT, kR, heater ends z1, z2 (prior sd
0.1 cm), thermistor depths (prior 0.05 cm), x spacing (prior 0.03 cm),
gains (prior 2 %), amplitude free.  5 mK at 25 Hz, 1 pulse.

Output: results/radial_flow.json
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
from scipy.special import erf

import vecmodel as vm

HERE = Path(__file__).resolve().parent
T = np.arange(1.5, 180.0 + 1e-9, 0.5)
W = vm.FS * 0.5


def response(p, design):
    (ux, uz, lkL, lkT, lkR, z1, z2, lA) = p[:8]
    kL, kT, kR = np.exp(lkL), np.exp(lkT), np.exp(lkR)
    ux, uz = ux / 3600.0, uz / 3600.0
    rest = p[8:]
    out = []
    n = 0
    src = 0.5 * vm.DUR * (vm.GL_X + 1.0)
    w = 0.5 * vm.GL_W
    tau = T[:, None] - src[None, :]
    for xi, x0 in enumerate((0.6, -0.6)):
        dxs = rest[xi]
        for di, z in enumerate(design["depths"]):
            dz = rest[2 + 3 * xi + di]
            g = np.exp(rest[8 + 3 * xi + di])
            x = x0 + dxs
            zz = z + dz
            q = (x - ux * tau) ** 2 / kL
            base = np.exp(-q / (4 * tau)) / tau / np.sqrt(kL * kT)
            s = 2 * np.sqrt(kR * tau)
            F = 0.5 * (erf((zz - z1 - uz * tau) / s) - erf((zz - z2 - uz * tau) / s))
            out.append(g * np.exp(lA) * (base * F) @ w)
            n += 1
    return np.concatenate(out)


def response_reflect(p, design, refl):
    """Truth with a partially reflecting outer boundary at z = 0 (bark):
    image heater mirrored about z = 0 with weight refl (1 = adiabatic)."""
    q = p.copy()
    y = response(p, design)
    q[5], q[6] = -design["z2"], -design["z1"]
    return y + refl * response(q, design)


def fitted_uz_bias(design, refl, ux=10.0):
    """MAP fit (infinite-medium model with uz free) to noise-free truth that
    has a reflecting bark boundary and no radial flow."""
    from scipy.optimize import least_squares
    p0 = np.array([ux, 0.0, np.log(0.0025), np.log(0.0025 / 1.85), np.log(0.0025 / 1.85),
                   design["z1"], design["z2"], np.log(vm.A_REF)] + [0.0] * 14)
    y = response_reflect(p0, design, refl)
    sig = vm.SIGMA_25HZ / np.sqrt(W)
    psd = np.zeros(len(p0))
    psd[5:7] = 0.1
    psd[8:10] = 0.03
    psd[10:16] = 0.05
    psd[16:22] = 0.02
    sel = psd > 0

    def r(p):
        return np.concatenate(((response(p, design) - y) / sig, (p[sel] - p0[sel]) / psd[sel]))

    res = least_squares(r, p0, x_scale="jac")
    return float(res.x[1]), float(res.x[0]), float(1e3 * np.sqrt(np.mean((response(res.x, design) - y) ** 2)))


def crb_uz(design, ux=10.0, uz=0.0):
    p = np.array([ux, uz, np.log(0.0025), np.log(0.0025 / 1.85), np.log(0.0025 / 1.85),
                  design["z1"], design["z2"], np.log(vm.A_REF)] + [0.0] * 2 + [0.0] * 6 + [0.0] * 6)
    n = len(p)
    J = []
    for i in range(n):
        h = 1e-4 if i not in (0, 1) else 1e-3
        dp = np.zeros(n)
        dp[i] = h
        J.append((response(p + dp, design) - response(p - dp, design)) / (2 * h))
    J = np.array(J).T
    F = J.T @ J * W / vm.SIGMA_25HZ**2
    prior = np.zeros(n)
    prior[5] = prior[6] = 1 / 0.1**2
    prior[8:10] = 1 / 0.03**2
    prior[10:16] = 1 / 0.05**2
    prior[16:22] = 1 / 0.02**2
    Fp = F + np.diag(prior)
    C = np.linalg.inv(Fp)
    Cn = np.linalg.inv(F + np.diag(np.where(prior > 0, 1e8, 0)))  # all geometry known exactly
    return float(np.sqrt(C[1, 1])), float(np.sqrt(Cn[1, 1])), float(np.sqrt(C[0, 0]))


if __name__ == "__main__":
    designs = {
        "full_heater_3depths": dict(depths=(0.5, 1.75, 3.0), z1=0.0, z2=3.5),
        "thermistor_beyond_heater_end": dict(depths=(0.5, 1.75, 3.0), z1=0.0, z2=2.7),
    }
    res = {}
    for name, d in designs.items():
        for ux in (2.0, 10.0):
            s, s_known, s_ux = crb_uz(d, ux=ux)
            res[f"{name}_ux{ux:g}"] = dict(sd_uz_cm_h=s, sd_uz_geometry_known=s_known, sd_ux_cm_h=s_ux)
            print(f"{name:32s} ux={ux:4.1f} cm/h: sd(uz) = {s:7.2f} cm/h (geometry exact: {s_known:6.2f}); sd(ux) = {s_ux:.2f}")
    # How big is the radial-flow signal at all?  Peak change in the deepest
    # thermistor for uz = 1 cm/h (a large radial velocity).
    d = designs["full_heater_3depths"]
    p0 = np.array([10.0, 0.0, np.log(0.0025), np.log(0.0025 / 1.85), np.log(0.0025 / 1.85), 0.0, 3.5, np.log(vm.A_REF)] + [0.0] * 14)
    p1 = p0.copy()
    p1[1] = 1.0
    dy = (response(p1, d) - response(p0, d)).reshape(6, -1)
    res["signal_mK_uz1_by_channel"] = [float(1e3 * np.abs(r).max()) for r in dy]
    print("max |dT| per channel for uz = 1 cm/h (mK):", np.round(res["signal_mK_uz1_by_channel"], 3))
    for refl in (0.25, 1.0):
        b, uxh, rm = fitted_uz_bias(d, refl)
        res[f"bias_reflect{refl:g}"] = dict(uz_fit_cm_h=b, ux_fit_cm_h=uxh, rmse_mK=rm)
        print(f"bark boundary reflection {refl:4.2f}, true uz = 0: fitted uz = {b:+.2f} cm/h, ux = {uxh:.2f} (true 10), residual {rm:.2f} mK")
    (HERE / "results").mkdir(exist_ok=True)
    (HERE / "results/radial_flow.json").write_text(json.dumps(res, indent=1))
