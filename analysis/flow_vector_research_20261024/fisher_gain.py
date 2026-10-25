"""Where the flow information actually lives: channel gains vs heater power.

README sec. 2.2 shows that for an ideal line source (finite pulse or not)
the kernel factorises per pair as

    S_hj(t) = exp(b_hj / 2) * F(a_hj, c, t),   b_hj = (p_j - p_h)^T K^-1 u,

so the flow-asymmetry term b is an amplitude, exactly confounded with
g_j * A_h (thermistor gain x heater amplitude), and the |u|^2 decay c is
exactly confounded with a common-mode loss.  Because b_hj = phi(p_j) -
phi(p_h) with phi(p) = p^T K^-1 u / 2, even reciprocal (dual-role) pairs
cannot separate it from free per-needle gains.  What can: a prior on the
heater amplitudes A_h relative to one another (heater energy is measured
electrically, and conduction lag does not change the energy delivered),
because then the heater-side term -phi(p_h) is pinned and three non-collinear
heaters give both components of K^-1 u without trusting any thermistor gain.

This script checks that numerically: Bayesian CRB of flow angle and speed
(realistic model: blur + loss free, positions 0.03 cm) vs the thermistor
gain prior (0.5 %, 2 %, 10 %, 50 %, effectively free) with the heater
amplitude ratios free or known to 1 % / 0.2 %, flow 25 deg off the probe
axis, 10 and 2 cm/h, with and without the finite-needle blur term (blur
breaks the factorisation weakly and lends dual-role layouts some gain-free
information even without a heater prior; that second-order effect should
not be relied on, so the figure uses the no-blur model).

Output: results/fisher_gain.json, fig_gain_vs_heater.png
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pandas as pd

import vecmodel as vm

HERE = Path(__file__).resolve().parent
KL = 0.0025


def run(lay, phi, U, sig_gain, sig_Aratio, sig_pos=0.03, aligned=False, blur=True):
    m = vm.Model(lay, aligned=aligned, blur=blur, loss=True)
    ph = np.deg2rad(phi)
    p = m.theta0(kL=KL, kT=KL / 1.85, th=ph, ux=U * np.cos(ph), uy=U * np.sin(ph))
    P = m.prior_precision(sig_pos, sig_gain)
    if sig_Aratio is not None and m.H > 1:
        i0 = m.idx["lA0"]
        for h in range(1, m.H):
            v = np.zeros(len(p))
            v[m.idx[f"lA{h}"]], v[i0] = 1.0, -1.0
            P += np.outer(v, v) / sig_Aratio**2
    if sig_gain >= 0.5:
        # A common shift of every gain against every heater amplitude is an
        # exact symmetry of the data that does not touch the flow; pin it
        # (gain of channel 0) so a loose gain prior is not mistaken for a
        # null direction by the eigenvalue cutoff in vecmodel.crb.
        i = m.idx["lg0"]
        P[i, i] += 1e8
    r = vm.crb(m, p, P)
    return dict(phi=r["phi"], speed_rel=r["speed_rel"])


def main():
    L = vm.layouts()
    rows = []
    for name in ("std_axial", "tri3", "Y3", "hex6", "2H_tri", "dual2", "dual3", "dual4"):
        lay = L[name]
        for U in (10.0, 2.0):
            for sg in (0.005, 0.02, 0.1, 0.5, 10.0):
                for sa in (None, 0.01, 0.002):
                    if sa is not None and len(lay.heater_ids) == 1:
                        continue
                    for sp in (0.03, 1e-3):
                        for blur in (True, False):
                            r = run(lay, 25.0, U, sg, sa, sp, blur=blur)
                            rows.append(dict(layout=name, needles=lay.n_needles, heaters=len(lay.heater_ids), U=U,
                                             sig_gain=sg, sig_Aratio=sa if sa else np.nan, sig_pos=sp, blur=blur,
                                             phi_sd=r["phi"], speed_rel=r["speed_rel"]))
    df = pd.DataFrame(rows)
    (HERE / "results/fisher_gain.json").write_text(df.to_json(orient="records", indent=1))
    pd.set_option("display.width", 200)
    df["sig_Aratio"] = df.sig_Aratio.fillna(-1.0)  # -1 = heater amplitudes free
    for sp in (0.03, 1e-3):
        for blur in (True, False):
            sel = (df.sig_pos == sp) & (df.blur == blur)
            print(f"\nsig_pos = {sp} cm, blur term in the model: {blur}  (sig_Aratio -1 = heater amplitudes free)")
            for col, nd, lab in (("phi_sd", 2, "flow-angle sd (deg)"), ("speed_rel", 3, "speed sd (relative)")):
                t = df[sel].pivot_table(index=["layout", "needles", "heaters", "sig_Aratio"], columns=["U", "sig_gain"], values=col)
                print(lab)
                print(t.round(nd).to_string())
    plot(df)


def plot(df):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    fig, axs = plt.subplots(1, 2, figsize=(11, 4.2), constrained_layout=True)
    d = df[(df.sig_pos == 0.03) & (df.blur == False) & (df.sig_gain <= 0.5)].copy()  # noqa: E712
    d["sig_Aratio"] = d.sig_Aratio.replace(-1.0, np.nan)
    d.loc[d.phi_sd > 1000, "phi_sd"] = np.nan
    sty = {"tri3": ("C0", "-"), "hex6": ("C2", "-"), "2H_tri": ("C1", "-"), "dual3": ("C3", "-"), "dual4": ("C4", "-")}
    for ax, U in zip(axs, (2.0, 10.0)):
        for name, (col, _) in sty.items():
            for sa, ls in ((np.nan, "--"), (0.01, "-")):
                s = d[(d.layout == name) & (d.U == U)]
                s = s[s.sig_Aratio.isna()] if np.isnan(sa) else s[s.sig_Aratio == sa]
                if s.empty:
                    continue
                lab = f"{name} ({s.needles.iloc[0]} needles)" + ("" if np.isnan(sa) else ", heater ratio 1 %")
                if np.isnan(sa) and s.heaters.iloc[0] > 1:
                    lab = f"{name}, heater amplitudes free"
                ax.plot(100 * s.sig_gain, s.phi_sd, "o" + ls, color=col, label=lab, ms=4)
        ax.set_xscale("log"); ax.set_yscale("log")
        ax.set_xlabel("thermistor gain prior sd (%)")
        ax.set_ylabel("flow-angle sd (deg), flow 25 deg off axis")
        ax.set_title(f"{U:g} cm/h")
        ax.grid(alpha=0.3, which="both")
    axs[0].legend(fontsize=7)
    fig.suptitle("Flow-angle CRB vs thermistor gain prior (no blur term, loss free, positions 0.03 cm): "
                 "dual-role needles with known heater-energy ratios do not need gains")
    fig.savefig(HERE / "fig_gain_vs_heater.png", dpi=130)


if __name__ == "__main__":
    import sys
    if len(sys.argv) > 1 and sys.argv[1] == "plot":
        plot(pd.read_json(HERE / "results/fisher_gain.json"))
    else:
        main()
