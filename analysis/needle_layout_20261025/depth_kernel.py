"""How far along the needle does a thermistor 'see'?  Justifies (and bounds)
treating each depth as an independent 2-D slice in Study B.

1. Geometry.  For a straight, infinitely long line heater in wood whose
   properties do not vary with depth z, integrating the 3-D point kernel
   along the heater gives exactly the 2-D line-source solution at the
   thermistor's in-plane offset, for any tilt of the sensor hole (the
   thermistor is a point).  So a drifted sensor hole is exactly a set of 2-D
   slices with local spacing.  Checked numerically below.

2. Flow profile.  With V(z) varying, the heat reaching a thermistor at
   depth z at time t has wandered in z.  Because the heater is uniform in z,
   the backward-in-time depth of that heat is free diffusion from z:
   Gaussian with variance 2 k_r tau after tau seconds.  The advection a pulse
   picks up is the time average over tau in [0, t] of V at that depth, so
   the effective depth kernel is the mixture
       K_t(dz) = (1/t) int_0^t N(dz; 0, 2 k_r tau) dtau,
   variance k_r t, sharply peaked.  Its FWHM is reported for k_r = kT
   (radial ~ tangential) and kL, and the Study B day profile is blurred with
   it to bound the slice error against the 0.5 cm/h / 5 % criterion.

3. Heater ends.  A 12 cm heater with an insulating bark surface (mirror) and
   a deep end at 12 cm: the time-dependent end factor at the deepest
   thermistors.

Output: results/depth_kernel.json, fig_b_kernel.png
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
from scipy.special import erf

import study_b as B

HERE = Path(__file__).resolve().parent
KL = 0.0025
KT = KL / 1.85


def kernel(dz, t, k, n=4000):
    tau = (np.arange(n) + 0.5) / n * t
    s2 = 2 * k * tau
    return np.mean(np.exp(-dz[:, None] ** 2 / (2 * s2[None, :])) / np.sqrt(2 * np.pi * s2[None, :]), axis=1)


def fwhm(dz, f):
    h = f.max() / 2
    above = dz[f >= h]
    return float(above.max() - above.min())


def main():
    out = {}
    # 1. line integral of the 3-D kernel along a straight heater = 2-D kernel
    k, t, r = KT, 40.0, 0.62
    zp = np.linspace(-20, 20, 400001)
    g3 = np.exp(-(r**2 + zp**2) / (4 * k * t)) / (4 * np.pi * k * t) ** 1.5
    g2 = np.exp(-r**2 / (4 * k * t)) / (4 * np.pi * k * t)
    out["line_integral_vs_2d_rel_err"] = float(abs(np.trapezoid(g3, zp) / g2 - 1))

    dz = np.linspace(-1.5, 1.5, 6001)
    rows = []
    for kk, lab in ((KT, "kT"), (KL, "kL")):
        for t in (20.0, 40.0, 60.0, 120.0):
            f = kernel(dz, t, kk)
            rows.append(dict(k=lab, t=t, fwhm_mm=10 * fwhm(dz, f), sd_mm=10 * float(np.sqrt(kk * t)),
                             frac_within_1p5mm=float(np.trapezoid(f[np.abs(dz) <= 0.15], dz[np.abs(dz) <= 0.15]))))
    out["kernel"] = rows

    # 2. blur the day profile with the t = 40 and 60 s kernels (radial k = kT)
    zf = np.linspace(-2, 14, 16001)
    vf = B.vprofile(zf, 15.0)
    blur = {}
    for t in (40.0, 60.0):
        kz = kernel(np.linspace(-1.5, 1.5, 3001), t, KT)
        kz /= kz.sum()
        vb = np.convolve(vf, kz, mode="same")
        vz = np.interp(B.Z, zf, vb)
        v0 = B.vprofile(B.Z, 15.0)
        err = vz - v0
        blur[f"{t:g}"] = dict(max_abs_err=float(np.abs(err).max()), z_at_max=float(B.Z[np.argmax(np.abs(err))]),
                              max_err_over_thr=float(np.max(np.abs(err) / B.thresh(v0))),
                              err=err.round(4).tolist())
    out["profile_blur_day15"] = blur

    # 3. heater-end factor (insulated bark at z = 0, heater 0..L)
    Lh = 12.0
    ends = []
    for z in (0.15, 0.45, 11.25, 11.55, 11.85):
        for t in (20.0, 60.0, 120.0):
            s = np.sqrt(4 * KT * t)
            fac = 0.5 * (erf((Lh - z) / s) + erf((Lh + z) / s))
            ends.append(dict(z=z, t=t, factor=float(fac)))
    out["heater_end_factor"] = ends
    (HERE / "results/depth_kernel.json").write_text(json.dumps(out, indent=1))
    for k_, v in out.items():
        if k_ != "profile_blur_day15":
            print(k_, v)
    print({k_: {kk: vv for kk, vv in v.items() if kk != "err"} for k_, v in blur.items()})

    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    fig, axs = plt.subplots(1, 2, figsize=(11, 3.8), constrained_layout=True)
    for t in (20.0, 60.0):
        f = kernel(dz, t, KT)
        axs[0].plot(10 * dz, f / f.max(), label=f"t = {t:g} s, k_r = kT")
    axs[0].set(xlabel="depth offset (mm)", ylabel="relative weight", title="Depth kernel of the advection a thermistor sees",
               xlim=(-8, 8))
    axs[0].legend(); axs[0].grid(alpha=0.3)
    v0 = B.vprofile(B.Z, 15.0)
    axs[1].plot(B.Z, v0, "k.-", label="slice V(z), day profile")
    axs[1].plot(B.Z, v0 + np.array(blur["60"]["err"]), "C1.-", label="blurred with the 60 s kernel")
    axs[1].set(xlabel="depth below bark surface (cm)", ylabel="V (cm/h)", title="Slice error from the flow-profile blur")
    axs[1].legend(); axs[1].grid(alpha=0.3)
    fig.savefig(HERE / "fig_b_kernel.png", dpi=120)


if __name__ == "__main__":
    main()
