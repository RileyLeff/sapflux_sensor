"""Information per needle under the realistic fit model.

Same Bayesian CRB as fisher.py, but with the two terms real data demand
(real_capture.py): the finite-needle blur c (free) and a common-mode loss
rate per heater (free; it removes the |u|^2 decay information).  Every
layout is scanned over flow angle 0..165 deg (15 deg steps; the general
model's bounds repeat every 180 deg up to sign for the symmetric layouts and
are close for the rest) at 2 and 10 cm/h, grain = flow (R = 1.85), needle
positions 0.03 cm, gains 2 %, one pulse per heater.

Added families, to see where another needle stops paying:
  ringN    1 heater + N thermistors on a 0.6 cm circle, phase chosen so no
           thermistor is directly up/downstream of the heater (N = 2..8)
  dualN    every needle heats and senses; dual2/3/4 from vecmodel.layouts

Also reported: "pulses" (heater firings per measurement, i.e. energy and
time), and information per needle 1/(n sd^2), normalised to tri3.

Output: results/fisher_needles.json, fig_needles_info.png
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pandas as pd

import vecmodel as vm

HERE = Path(__file__).resolve().parent
KL = 0.0025


def ring_layouts(s=vm.S_REF):
    out = {}
    for n in range(2, 9):
        # rotate so the nearest thermistor sits half a step off the axial line
        phase = 180.0 / n if n % 2 == 0 else 90.0
        out[f"ring{n}"] = vm.Layout(f"ring{n}", np.zeros((1, 2)), vm.ring(n, s, phase), f"1 heater + {n} thermistors")
    return out


def run(lay, aligned, phi, U):
    m = vm.Model(lay, aligned=aligned, blur=True, loss=True)
    ph = np.deg2rad(phi)
    p = m.theta0(kL=KL, kT=KL / 1.85, th=ph, ux=U * np.cos(ph), uy=U * np.sin(ph))
    return vm.crb(m, p, m.prior_precision(0.03, 0.02))


def main():
    L = vm.layouts()
    lays = {k: L[k] for k in ("std_axial", "T3_axial+side", "cross4", "tri3", "tri3_rot", "Y3", "hex6", "pent5",
                              "2H_tri", "2H_diamond", "dual2", "dual3", "dual4")}
    lays.update(ring_layouts())
    rows = []
    for name, lay in lays.items():
        for aligned in (False, True):
            for U in (2.0, 10.0):
                for a in np.arange(0, 180, 15):
                    r = run(lay, aligned, float(a), U)
                    rows.append(dict(layout=name, aligned=aligned, U=U, phi_true=float(a), needles=lay.n_needles,
                                     pulses=len(lay.heater_ids), pairs=len(lay.pairs), **r))
        print("done", name, flush=True)
    df = pd.DataFrame(rows)
    df.to_json(HERE / "results/fisher_needles.json", orient="records")
    g = df.replace(np.inf, np.nan).groupby(["layout", "aligned", "U"])
    summ = g.agg(needles=("needles", "first"), pulses=("pulses", "first"), pairs=("pairs", "first"),
                 phi_med=("phi", "median"), phi_max=("phi", "max"), speed_med=("speed_rel", "median"),
                 kL_med=("kL_rel", "median"), grain_med=("theta", "median")).reset_index()
    ninf = df.groupby(["layout", "aligned", "U"]).phi.apply(lambda s: int(np.isinf(s).sum())).reset_index(name="n_inf")
    summ = summ.merge(ninf)
    ref = summ[(summ.layout == "tri3")].set_index(["aligned", "U"]).phi_med
    summ["info_per_needle_vs_tri3"] = [
        (ref[(a, u)] ** 2 * 4) / (pm**2 * n) if np.isfinite(pm) else 0.0
        for a, u, pm, n in zip(summ.aligned, summ.U, summ.phi_med, summ.needles)
    ]
    pd.set_option("display.width", 200)
    print(summ.round(3).sort_values(["aligned", "U", "needles"]).to_string(index=False))
    (HERE / "results/fisher_needles_summary.json").write_text(summ.to_json(orient="records", indent=1))

    plot(summ)


def plot(summ):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.2), constrained_layout=True)
    for ax, aligned in zip(axs, (False, True)):
        for U, mk in ((10.0, "o"), (2.0, "s")):
            s = summ[(summ.aligned == aligned) & (summ.U == U) & (summ.n_inf == 0)]
            ring = s[s.layout.str.startswith("ring")].sort_values("needles")
            ax.plot(ring.needles, ring.phi_med, mk + "-", color="C0", label=f"ringN, {U:g} cm/h")
            dual = s[s.layout.str.startswith("dual")].sort_values("needles")
            ax.plot(dual.needles, dual.phi_med, mk + "-", color="C3", label=f"dualN, {U:g} cm/h")
            for _, r in s[~s.layout.str.startswith(("ring", "dual"))].iterrows():
                if np.isfinite(r.phi_med) and r.n_inf == 0:
                    ax.plot(r.needles, r.phi_med, mk, color="0.4", ms=4)
                    if U == 10.0:
                        ax.annotate(r.layout, (r.needles, r.phi_med), fontsize=7, xytext=(3, 2), textcoords="offset points")
        ax.set_yscale("log")
        ax.set_xlabel("needles in the stem")
        ax.set_ylabel("median flow-angle sd (deg)")
        ax.set_title("general model (flow may cross grain)" if not aligned else "aligned model (flow along grain)")
        ax.grid(alpha=0.3, which="both")
        ax.legend(fontsize=7)
    fig.suptitle("CRB with blur + common-mode loss free; positions 0.03 cm, gains 2 %; layouts unidentifiable at any angle omitted")
    fig.savefig(HERE / "fig_needles_info.png", dpi=130)


if __name__ == "__main__":
    import sys
    if len(sys.argv) > 1 and sys.argv[1] == "plot":
        plot(pd.read_json(HERE / "results/fisher_needles_summary.json"))
    else:
        main()
