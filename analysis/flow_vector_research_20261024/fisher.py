"""Cramer-Rao (Bayesian) bounds on flow angle, speed, diffusivities and
grain angle for candidate needle layouts, as a function of the true flow
direction.

Truth: kL = 0.0025 cm^2/s, kT = kL / R (R = 1.85 unless swept), the flow
runs along the grain at angle phi from the probe x axis, speed U.  Priors:
needle positions N(0, sig_pos) per coordinate on every needle (heaters too),
channel gains N(0, sig_gain) in log, heater amplitudes and diffusivities
free, linear baseline per channel and pulse fitted from 60 s of pre-pulse
data.  White noise 5 mK per 25 Hz sample, 1 pulse per heater.

Two fit models:
  general  u = (ux, uy) free, independent of the grain axis (wounds,
           junctions: flow need not follow the grain)
  aligned  u along the grain axis (spiral grain, crooked install)

Output: results/fisher_*.json, fig_fisher_angle.png, fig_fisher_sweeps.png
"""

from __future__ import annotations

import json
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

import vecmodel as vm

HERE = Path(__file__).resolve().parent
OUT = HERE / "results"
OUT.mkdir(exist_ok=True)

KL = 0.0025


def run_case(lay, aligned, phi_deg, U, R=1.85, sig_pos=0.03, sig_gain=0.02, ppH=1.0, sig_lR=None):
    m = vm.Model(lay, aligned=aligned)
    ph = np.deg2rad(phi_deg)
    p = m.theta0(kL=KL, kT=KL / R, th=ph, ux=U * np.cos(ph), uy=U * np.sin(ph))
    P = m.prior_precision(sig_pos=sig_pos, sig_gain=sig_gain, sig_lR=sig_lR)
    if aligned and U == 0.0:
        p[m.idx["U"]] = 1e-6
    return vm.crb(m, p, P, pulses_per_heater=ppH)


def angle_scan():
    lays = vm.layouts()
    angles = np.arange(0, 360, 15)
    rows = []
    for name, lay in lays.items():
        for aligned in (False, True):
            for U in (0.0, 2.0, 10.0, 30.0):
                if U == 0.0 and not aligned:
                    continue
                for a in angles:
                    for geom in ("realistic", "perfect"):
                        sp, sg = (0.03, 0.02) if geom == "realistic" else (1e-4, 1e-4)
                        # "cycle": every heater fires once.  "equal": every
                        # layout spends two pulses of energy per cycle, so a
                        # single-heater layout fires twice and averages.
                        for energy in ("cycle", "equal"):
                            ppH = 2.0 if (energy == "equal" and len(lay.heaters) == 1) else 1.0
                            r = run_case(lay, aligned, float(a), U, sig_pos=sp, sig_gain=sg, ppH=ppH)
                            rows.append(dict(layout=name, aligned=aligned, U=U, phi_true=float(a), geom=geom,
                                             energy=energy, needles=lay.n_needles, heaters=len(lay.heaters), **r))
        print("done", name, flush=True)
    df = pd.DataFrame(rows)
    df.to_json(OUT / "fisher_angle.json", orient="records")
    return df


def summarize(df):
    """Per layout / model / speed: median and 90th percentile over true flow
    angles, and the fraction of angles where the angle is known to < 10 deg."""
    out = []
    for (lay, al, U, ee), sub in df.groupby(["layout", "aligned", "U", "energy"]):
        r = sub[sub.geom == "realistic"]
        pf = sub[sub.geom == "perfect"]
        row = dict(layout=lay, aligned=al, U=U, energy=ee, needles=int(r.needles.iloc[0]),
                   heaters=int(r.heaters.iloc[0]))
        for col in ("phi", "speed_rel", "theta", "kL_rel", "kT_rel"):
            v = r[col].to_numpy(float)
            v = np.where(np.isfinite(v), v, 1e9)
            row[col + "_med"] = float(np.median(v))
            row[col + "_p90"] = float(np.percentile(v, 90))
        row["frac_phi_lt10"] = float(np.mean(r.phi.to_numpy(float) < 10.0))
        vp = np.where(np.isfinite(pf.phi), pf.phi, 1e9)
        row["phi_med_perfect"] = float(np.median(vp))
        vs = np.where(np.isfinite(pf.speed_rel), pf.speed_rel, 1e9)
        row["speed_rel_med_perfect"] = float(np.median(vs))
        out.append(row)
    s = pd.DataFrame(out)
    s.to_json(OUT / "fisher_summary.json", orient="records", indent=1)
    return s


def sweeps():
    lays = vm.layouts
    rows = []
    angles = np.arange(0, 180, 15)
    for s in (0.4, 0.5, 0.6, 0.8, 1.0):
        L = vm.layouts(s)
        for name in ("tri3", "cross4", "hex6", "T3_axial+side", "2H_tri"):
            for U in (2.0, 10.0):
                for aligned in (False, True):
                    v = [run_case(L[name], aligned, float(a), U) for a in angles]
                    rows.append(dict(sweep="spacing", x=s, layout=name, U=U, aligned=aligned,
                                     phi_med=float(np.median([r["phi"] for r in v])),
                                     speed_med=float(np.median([r["speed_rel"] for r in v])),
                                     theta_med=float(np.median([r["theta"] for r in v]))))
    L = vm.layouts()
    for sp in (0.005, 0.01, 0.02, 0.03, 0.045, 0.06):
        for name in ("tri3", "cross4", "hex6", "T3_axial+side", "2H_tri", "2H_diamond"):
            for U in (2.0, 10.0):
                for aligned in (False, True):
                    v = [run_case(L[name], aligned, float(a), U, sig_pos=sp) for a in angles]
                    rows.append(dict(sweep="sig_pos", x=sp, layout=name, U=U, aligned=aligned,
                                     phi_med=float(np.median([r["phi"] for r in v])),
                                     speed_med=float(np.median([r["speed_rel"] for r in v])),
                                     theta_med=float(np.median([r["theta"] for r in v]))))
    for R in (1.0, 1.25, 1.5, 1.85, 2.2, 2.5):
        for name in ("tri3", "cross4", "hex6", "T3_axial+side"):
            for U in (0.0, 2.0, 10.0):
                for aligned in (False, True):
                    if U == 0.0 and not aligned:
                        continue
                    v = [run_case(L[name], aligned, float(a), U, R=R) for a in angles]
                    rows.append(dict(sweep="R", x=R, layout=name, U=U, aligned=aligned,
                                     phi_med=float(np.median([r["phi"] for r in v])),
                                     speed_med=float(np.median([r["speed_rel"] for r in v])),
                                     theta_med=float(np.median([r["theta"] for r in v]))))
    df = pd.DataFrame(rows)
    df.to_json(OUT / "fisher_sweeps.json", orient="records", indent=1)
    return df


def plots(df, sw):
    show = ["std_axial", "T3_axial+side", "cross4", "tri3", "hex6", "2H_tri"]
    colors = dict(zip(show, ("#444", "#E69F00", "#3B82F6", "#B91C1C", "#166534", "#7C3AED")))
    fig, axes = plt.subplots(2, 2, figsize=(12, 8), sharex=True)
    for (i, aligned), (j, U) in [((i, a), (j, u)) for i, a in enumerate((True, False)) for j, u in enumerate((2.0, 10.0))]:
        ax = axes[i, j]
        for name in show:
            d = df[(df.layout == name) & (df.aligned == aligned) & (df.U == U) & (df.geom == "realistic") & (df.energy == "cycle")]
            y = np.where(np.isfinite(d.phi), d.phi, 200.0)
            ax.plot(d.phi_true, np.minimum(y, 200.0), "-o", ms=3, color=colors[name], label=name)
        ax.set_yscale("log")
        ax.set_ylim(0.5, 200)
        ax.axhline(10, color="#999", lw=0.8, ls="--")
        ax.set_title(f"{'flow along grain (aligned)' if aligned else 'flow independent of grain (general)'}, U = {U:g} cm/h", fontsize=10)
        ax.set_ylabel("sd of flow angle (deg)")
        ax.grid(alpha=0.3, which="both")
    for ax in axes[1]:
        ax.set_xlabel("true flow angle from probe x axis (deg)")
    axes[0, 0].legend(fontsize=8, ncol=2)
    fig.suptitle("Bayesian CRB on flow direction, 0.6 cm spacing, positions +-0.03 cm, gains +-2%, 5 mK @ 25 Hz", fontsize=10)
    fig.tight_layout()
    fig.savefig(HERE / "fig_fisher_angle.png", dpi=140)

    fig, axes = plt.subplots(1, 3, figsize=(15, 4.3))
    for ax, sweep, xl in zip(axes, ("spacing", "sig_pos", "R"), ("needle spacing s (cm)", "needle position sd (cm)", "anisotropy R = kL/kT")):
        for name in ("tri3", "hex6", "cross4", "T3_axial+side", "2H_tri"):
            for aligned, ls in ((True, "-"), (False, ":")):
                d = sw[(sw.sweep == sweep) & (sw.layout == name) & (sw.U == 10.0) & (sw.aligned == aligned)].sort_values("x")
                if len(d) == 0:
                    continue
                ax.plot(d.x, np.minimum(d.phi_med, 200), ls, marker="o", ms=3, color=colors[name],
                        label=f"{name} {'aligned' if aligned else 'general'}")
        ax.set_yscale("log")
        ax.set_xlabel(xl)
        ax.set_ylabel("median sd of flow angle at 10 cm/h (deg)")
        ax.grid(alpha=0.3, which="both")
    axes[0].legend(fontsize=7, ncol=2)
    fig.tight_layout()
    fig.savefig(HERE / "fig_fisher_sweeps.png", dpi=140)


if __name__ == "__main__":
    df = angle_scan()
    s = summarize(df)
    pd.set_option("display.width", 220)
    pd.set_option("display.max_columns", 30)
    cols = ["layout", "aligned", "U", "energy", "needles", "heaters", "phi_med", "phi_p90", "frac_phi_lt10",
            "speed_rel_med", "speed_rel_p90", "theta_med", "kL_rel_med", "kT_rel_med", "phi_med_perfect",
            "speed_rel_med_perfect"]
    print(s[cols].round(3).to_string(index=False))
    sw = sweeps()
    print(sw.round(3).to_string(index=False))
    plots(df, sw)
