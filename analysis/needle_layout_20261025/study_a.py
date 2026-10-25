"""Study A: the "lowercase y" offset layout.

Sensor needles sit 0.6 cm up/down the grain from a 0.9 mm heater, but
shifted sideways (tangentially) by a common |offset| so that they leave the
heater's stagnant Darcy strip (README sec. A.1 here; needle_cost.py physics).
Every sensor has the same |offset|, so the point-symmetric up/down heat
ratio stays exp(x V / kL), independent of time.

Layouts (probe frame, x = axial/up the grain, y = tangential; cm):
  east30  today's collinear East30: 1.3 mm needles at 0, (+-0.6, 0), 2 mm wounds
  pair    0.9 mm heater at 0; 21G sensors at (0.6, +off) and (-0.6, -off)
          (point-symmetric, so no sensor sits in the other sensor's strip)
  y3      0.9 mm heater; 21G sensors at (0.6, +off), (0.6, -off), (-0.6, -off)
          ("lowercase y": two arms up, one stem down; the stem is the
          point-symmetric partner of the +off arm and shares an axial line
          with the -off arm)
Offsets: 0, 1.5a, 2a, 3a, 4a with a = 0.08 cm the heater's blocked half-width
(0.45 mm needle + 0.35 mm wound).  Wounds: "std" = needle + 0.35 mm per side;
"prop" = needle x 2/1.3 (sensitivity case).  Same absolute offsets for both.

FD: fd2d (needle_cost discretisation) at dx = 0.01 cm (needle_cost used
0.02; the offsets are 6-32 cells here), heat x in [-2, 2.6], y in [-1.5, 1.5],
Darcy x in [-12, 13].  Heater energy 4.8 J/cm.  Kp 1, 30, 300, 3000; probe
vertical and rotated 20 deg; 10 cm/h (plus 2 cm/h at Kp 300).

Fits (geometry fixed at truth, blur + loss free, as needle_cost "fixed"):
  axis     flow along the probe x axis, R = kL/kT = 1.85 known
           (all a 2-sensor pair supports; the East30 assumption)
  aligned  flow along a free grain angle (y3 only)
  general  flow free to cross the grain (y3 only)
  *_R      the same with R = 1.85 pinned (the y has only two pair
           orientations mod 180 deg, so K is not identifiable with R free;
           sec. A.4)
  hrm      heat-ratio method on the point-symmetric up/down pair, true kL
  fit_pair the project's 1-D estimator (East30 only, as in needle_cost)

Also: the first-order checks (Darcy speed-up beside the heater vs
y/sqrt(y^2 - a^2); no-flow signal loss at 60 s) and the Bayesian CRB of the
lowercase y next to the existing layouts (fisher_needles.py setup).

Output: results/study_a.json, results/study_a_firstorder.json,
results/study_a_crb.json, fig_a_*.png
"""

from __future__ import annotations

import json
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
import pandas as pd

import fd2d as F
import layoutlib as L
import needle_cost as nc
import vecmodel as vm

HERE = Path(__file__).resolve().parent
OUT = HERE / "results"
WORKERS = 8  # keep the laptop usable; run under `nice -n 15`
GRID = F.Grid(dx=0.01, hx=(-2.0, 2.6), y=(-1.5, 1.5), darcy_y=(-1.5, 1.5))
A = L.a_heater("std")  # 0.08 cm
OFFS = {"0": 0.0, "1.5a": 1.5 * A, "2a": 2 * A, "3a": 3 * A, "4a": 4 * A}
KPS = (1.0, 30.0, 300.0, 3000.0)


def c_blur(kind):
    _, _, rh, rs = L.study_a_layout(kind, 0.0)
    return (rh**2 + rs**2) / 8.0


def spec_for(args):
    kind, offk, beta, U, wmode, Kp = args
    h, s, rh, rs = L.study_a_layout(kind, OFFS[offk])
    ndl = L.fd_needles(h, s, rh, rs, "none" if wmode == "woodneedle" else ("e30" if kind == "east30" else wmode), beta)
    return dict(needles=ndl, heater=0, sensors=list(range(1, len(s) + 1)), U=U, Kp=Kp, grid=GRID, E=F.E_NEW,
                kind="wood" if wmode == "woodneedle" else "steel")


def key(args):
    return "|".join(map(str, args))


def simulate_all(J):
    """FD curves for every job, cached in results/study_a_curves.npz.  New
    slices go through the Rust time loop (fd2d.simulate_many); the cache's
    first entries were made with the identical Python loop."""
    cache = OUT / "study_a_curves.npz"
    have = dict(np.load(cache)) if cache.exists() else {}
    todo = [j for j in J if key(j) not in have]
    if todo:
        ys = F.simulate_many([spec_for(j) for j in todo], OUT / "_fdwork")
        have.update({key(j): y for j, y in zip(todo, ys)})
        np.savez_compressed(cache, **have)
    return {key(j): have[key(j)] for j in J}


def fit_case(args):
    (kind, offk, beta, U, wmode, Kp), y = args
    off = OFFS[offk]
    h, s, rh, rs = L.study_a_layout(kind, off)
    steel = wmode != "woodneedle"
    base = dict(layout=kind, offset=offk, offset_cm=off, beta=beta, phi_true=-beta, U=U, wound=wmode,
                Kp=Kp if steel else np.nan, peak_down_mK=float(1e3 * y[0].max()))
    rows = []
    lay = L.vm_layout(h, s)
    yy = y.ravel()
    c0 = c_blur(kind)
    kinds = ("axis",) if len(s) == 2 else ("axis", "aligned_R", "general_R", "aligned", "general")
    for k in kinds:
        rows.append(dict(**base, model=k, **L.fit_layout(lay, yy, k, -beta, U, c0)))
    dn, up = 0, (1 if len(s) == 2 else 2)  # sensor at (+0.6, +off) and its point-symmetric partner
    rows.append(dict(**base, model="hrm", speed_ratio=L.hrm(y[dn], y[up], L.S) / U, err_phi=np.nan, rmse_mK=np.nan))
    if kind == "east30":
        rows.append(dict(**base, model="fit_pair", **{k: v for k, v in nc.fit_1d(yy, U).items()}))
    return rows


def jobs():
    J = []
    for kind in ("east30", "pair", "y3"):
        offs = ["0"] if kind == "east30" else [o for o in OFFS if not (kind == "y3" and o == "0")]
        wms = ["std"] if kind == "east30" else ["std", "prop"]
        for offk in offs:
            J.append((kind, offk, 0.0, 10.0, "woodneedle", 1.0))
            for beta in (0.0, 20.0, -20.0):
                for wm in wms:
                    for Kp in KPS:
                        J.append((kind, offk, beta, 10.0, wm, Kp))
                J.append((kind, offk, beta, 2.0, "std", 300.0))
    return J


# ---------------------------------------------------------------------------
# First-order checks


def firstorder():
    rows = []
    for Kp in (30.0, 300.0, 3000.0):
        xs, ys, qx, qy = F.darcy([F.Needle(0, 0, L.R_H, A)], Kp, 1.0, GRID)
        ux = 0.5 * (qx[1:] + qx[:-1])
        for offk, off in OFFS.items():
            if off == 0:
                continue
            j = int(np.argmin(np.abs(ys - off)))
            i0 = int(np.argmin(np.abs(xs - 0.0)))
            i6 = int(np.argmin(np.abs(xs - L.S)))
            seg = ux[i0:i6 + 1, j].mean()
            rows.append(dict(Kp=Kp, offset=offk, y_cm=off, theory=float(off / np.sqrt(off**2 - A**2)),
                             fd_at_x0=float(ux[i0, j]), fd_at_sensor=float(ux[i6, j]), fd_mean_0_to_s=float(seg)))
    # no-flow signal at 60 s: offset sensor vs on-axis sensor, analytic line source
    sig = []
    for offk, off in OFFS.items():
        t = np.array([60.0])
        iso = vm.line_response(t, L.S, off, L.KL, L.KL, 0.0, 0.0, 0.0)[0] / vm.line_response(t, L.S, 0.0, L.KL, L.KL, 0.0, 0.0, 0.0)[0]
        ani = (vm.line_response(t, L.S, off, L.KL, L.KL / L.R_ANISO, 0.0, 0.0, 0.0)[0]
               / vm.line_response(t, L.S, 0.0, L.KL, L.KL / L.R_ANISO, 0.0, 0.0, 0.0)[0])
        sig.append(dict(offset=offk, y_cm=off, ratio_iso_k=float(iso), ratio_aniso_R185=float(ani),
                        closed_iso=float(np.exp(-off**2 / (4 * L.KL * 60.0))),
                        closed_aniso=float(np.exp(-off**2 / (4 * L.KL / L.R_ANISO * 60.0)))))
    return rows, sig


# ---------------------------------------------------------------------------
# CRB (fisher_needles.run with heater energy and needle blur as built)


def crb_layouts():
    Lv = vm.layouts()
    out = {k: (Lv[k], vm.C_NEEDLE) for k in ("std_axial", "T3_axial+side", "tri3", "Y3", "hex6", "dual3", "dual4")}
    cb = c_blur("pair")
    for offk, off in OFFS.items():
        h, s, _, _ = L.study_a_layout("pair", off)
        out[f"pair_{offk}"] = (L.vm_layout(h, s, f"pair_{offk}"), cb)
        if off > 0:
            h, s, _, _ = L.study_a_layout("y3", off)
            out[f"y3_{offk}"] = (L.vm_layout(h, s, f"y3_{offk}"), cb)
    return out


def crb_one(args):
    name, lay, c0, model, U, phi, E = args
    m = vm.Model(lay, aligned=(model != "general"), blur=True, loss=True)
    ph = np.deg2rad(phi)
    A_ = E / (4 * np.pi * F.C_WOOD)
    if model.endswith("_R"):
        m = vm.Model(lay, aligned=(model == "aligned_R"), blur=True, loss=True)
    if model == "axis":
        # the probe axis is taken as the flow axis: theta known (1e-3 rad), R known (1e-3)
        p = m.theta0(kL=L.KL, kT=L.KL / 1.85, th=0.0, ux=U, uy=0.0, A=A_)
        P = m.prior_precision(0.03, 0.02, sig_lR=1e-3)
        P[m.idx["th"], m.idx["th"]] += 1.0 / 1e-3**2
    else:
        p = m.theta0(kL=L.KL, kT=L.KL / 1.85, th=ph, ux=U * np.cos(ph), uy=U * np.sin(ph), A=A_)
        P = m.prior_precision(0.03, 0.02, sig_lR=1e-3 if model.endswith("_R") else None)
    p[m.idx["lc"]] = np.log(c0)
    r = vm.crb(m, p, P)
    return dict(layout=name, model=model, U=U, phi_true=phi, E_J_cm=E, needles=lay.n_needles, **r)


def crb_table():
    J = []
    for name, (lay, c0) in crb_layouts().items():
        for E in (F.E_NEW, float(vm.A_REF * 4 * np.pi * F.C_WOOD)):
            for U in (2.0, 10.0):
                J.append((name, lay, c0, "axis", U, 0.0, E))
                for model in ("general", "aligned", "general_R", "aligned_R"):
                    for a in np.arange(0, 180, 15):
                        J.append((name, lay, c0, model, U, float(a), E))
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        rows = list(ex.map(crb_one, J, chunksize=4))
    df = pd.DataFrame(rows)
    g = df.replace(np.inf, np.nan).groupby(["layout", "model", "U", "E_J_cm"])
    summ = g.agg(needles=("needles", "first"), phi_med=("phi", "median"), phi_max=("phi", "max"),
                 speed_med=("speed_rel", "median"), speed_max=("speed_rel", "max")).reset_index()
    ninf = df.groupby(["layout", "model", "U", "E_J_cm"]).phi.apply(lambda s: int(np.isinf(s).sum())).reset_index(name="n_inf_phi")
    ninf2 = df.groupby(["layout", "model", "U", "E_J_cm"]).speed_rel.apply(lambda s: int(np.isinf(s).sum())).reset_index(name="n_inf_speed")
    summ = summ.merge(ninf).merge(ninf2)
    summ["n_angles"] = df.groupby(["layout", "model", "U", "E_J_cm"]).size().values
    return df, summ


# ---------------------------------------------------------------------------


def plots(df, fo, sig=None):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    fig, axs = plt.subplots(3, 3, figsize=(14, 10.5), constrained_layout=True, sharey=True)
    d = df[(df.U == 10.0)]
    for col, beta in enumerate((0.0, 20.0, -20.0)):
        for row, (lay, model) in enumerate((("pair", "axis"), ("y3", "aligned_R"), ("y3", "general_R"))):
            ax = axs[row, col]
            for i, Kp in enumerate(KPS):
                s = d[(d.layout == lay) & (d.model == model) & (d.beta == beta) & (d.wound == "std") & (d.Kp == Kp)].sort_values("offset_cm")
                ax.plot(s.offset_cm / A, s.speed_ratio, "o-", color=f"C{i}", ms=4, label=f"Kp {Kp:g}")
                e = df[(df.layout == "east30") & (df.model == "axis") & (df.beta == beta) & (df.Kp == Kp) & (df.U == 10.0)]
                if len(e):
                    ax.axhline(e.speed_ratio.iloc[0], color=f"C{i}", ls=":", lw=1)
            ax.set_title(f"{lay} ({model} fit), install rotated {beta:g} deg")
            ax.set_xlabel("sideways offset / a  (a = 0.08 cm)")
            ax.set_ylabel("fitted / true speed")
            ax.axhline(1.0, color="k", lw=0.6)
            ax.set_ylim(0.45, 1.2)
            ax.grid(alpha=0.3)
    axs[0, 0].legend(fontsize=7, title="dotted: East30 1.3 mm collinear", title_fontsize=7)
    fig.suptitle("Study A: speed ratio vs sideways offset, geometry fixed at truth, 10 cm/h")
    fig.savefig(HERE / "fig_a_speed.png", dpi=120)

    fig, axs = plt.subplots(1, 2, figsize=(11, 4), constrained_layout=True)
    fo = pd.DataFrame(fo)
    for i, Kp in enumerate((30.0, 300.0, 3000.0)):
        s = fo[fo.Kp == Kp]
        axs[0].plot(s.y_cm / A, s.fd_at_x0, "o-", color=f"C{i}", label=f"FD at x = 0, Kp {Kp:g}")
        axs[0].plot(s.y_cm / A, s.fd_mean_0_to_s, "s--", color=f"C{i}", ms=3, label=f"FD mean over x in [0, s], Kp {Kp:g}")
    yy = np.linspace(1.3, 4.2, 100)
    axs[0].plot(yy, yy / np.sqrt(yy**2 - 1), "k-", label="flat plate y/sqrt(y^2 - a^2)")
    axs[0].set(xlabel="y / a", ylabel="axial flux / far field", title="Speed-up beside the 0.9 mm heater (+0.35 mm wound)")
    axs[0].legend(fontsize=7); axs[0].grid(alpha=0.3)
    if sig is not None:
        sg = pd.DataFrame(sig)
        axs[1].plot(sg.y_cm / A, sg.ratio_iso_k, "o-", label="isotropic k")
        axs[1].plot(sg.y_cm / A, sg.ratio_aniso_R185, "s-", label="kL/kT = 1.85 (sideways spread uses kT)")
        axs[1].set(xlabel="y / a", ylabel="T(60 s) offset / T(60 s) on axis", title="No-flow signal at 60 s, sensor 0.6 cm along grain")
        axs[1].legend(fontsize=8); axs[1].grid(alpha=0.3)
    plt.savefig(HERE / "fig_a_firstorder.png", dpi=120)


def main():
    J = jobs()
    curves = simulate_all(J)
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        rows = [r for rr in ex.map(fit_case, [(j, curves[key(j)]) for j in J]) for r in rr]
    df = pd.DataFrame(rows)
    df.to_json(OUT / "study_a.json", orient="records", indent=1)
    fo, sig = firstorder()
    (OUT / "study_a_firstorder.json").write_text(json.dumps(dict(speedup=fo, signal_60s=sig), indent=1))
    crb_df, crb_s = crb_table()
    crb_df.to_json(OUT / "study_a_crb_all.json", orient="records")
    crb_s.to_json(OUT / "study_a_crb.json", orient="records", indent=1)
    plots(df, fo, sig)
    pd.set_option("display.width", 250)
    print(df.round(3).to_string(index=False))
    print(pd.DataFrame(fo).round(3).to_string(index=False))
    print(pd.DataFrame(sig).round(4).to_string(index=False))
    print(crb_s.round(3).to_string(index=False))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "crb":
        _, s = crb_table()
        pd.set_option("display.width", 250)
        print(s.round(3).to_string(index=False))
    elif len(sys.argv) > 1 and sys.argv[1] == "fig":
        fo_ = json.loads((OUT / "study_a_firstorder.json").read_text())
        plots(pd.read_json(OUT / "study_a.json"), fo_["speedup"], fo_["signal_60s"])
    else:
        main()
