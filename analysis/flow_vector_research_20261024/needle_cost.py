"""What does each needle cost?  Darcy flow around non-conducting needles and
wounds, a finite-volume heat pulse in that flow, and the analytic vector
fit applied to the result.

Physics (derived in README sec. 6)
----------------------------------
Sap moves through the sapwood as Darcy flow in an anisotropic porous medium,
    q = -kappa grad p,   div q = 0,   kappa = diag(Kp, 1) in the grain frame,
where Kp = kappa_axial / kappa_tangential.  A needle plus its wound annulus
(radius a = w/2, w = wound width) is impermeable.  Stretching X = x/sqrt(Kp)
makes the problem isotropic and turns the disk into an ellipse of semi-axes
a/sqrt(Kp) (along the flow) by a (across): at large Kp, a flat plate normal
to the flow.  Its disturbance reaches |X| ~ a, i.e. |x| ~ a sqrt(Kp) in real
space, both upstream and downstream (potential flow is fore-aft symmetric:
there is no wake, but there is a stagnant strip on both sides).  So:

  * Kp = 1 (the implicit assumption of every potential-flow or 'wound
    annulus only' correction): the needle disturbs flow over ~ a, and flow
    speeds up to 2U at its flanks;
  * Kp >> 1: each needle kills a strip of width ~2a that extends ~ a sqrt(Kp)
    up and down the stem.  A needle directly up- or downstream of another
    needle within that distance sits in its shadow.  For a = 0.1 cm and 6 mm
    spacing the shadow covers the next needle once Kp > 36.

Kp for real sapwood is not something I could pin to a source I read during
this work; it is commonly quoted as orders of magnitude (axial conduits are
long, tangential exchange is through pits).  It is swept: 1, 30, 300, 3000.
Kp -> large is also the right limit when conduits are longer than the probe:
a vessel cut by the drill is dead along its whole length.

Numerics
--------
Darcy: 5-point finite volume, harmonic face permeabilities, impermeable
cells kappa = 1e-9, x in [-12, 13] cm, y in [-2, 2] cm, dx = 0.02 cm,
p = -(U/Kp) x on the left/right faces (the tree, not the probe, sets the
pressure gradient), no flux top/bottom.  The face fluxes are discretely
divergence-free and drive a conservative central-difference advection term
in the heat solve (x in [-2, 3], y in [-2, 2], same grid, Crank-Nicolson,
dt = 0.1 s, T = 0 outside).  Probe rotated by beta; flow along +x.

  wood    C = 2.8 J/cm^3/K, kL = 0.0025 cm^2/s, kT = kL/1.85
  needle  r = 0.065 cm, C = 3.0, conductivity 0.02 W/cm/K, no flow
  wound   annulus to r = w/2, no flow, isotropic conductivity at the
          tangential value (an assumption: dried wound tissue may conduct less)

Fits: the analytic vector model (blur + common-mode loss) in the general
and aligned forms, once with needle positions and gains fixed at truth
("fixed": isolates the physics bias) and once under the usual priors
(0.03 cm, 2 %: "prior"; on noise-free data any model mismatch then also
moves the fit along the prior-limited directions, so these rows scatter by
up to about the CRB); std_axial
gets the project's 1-D fit_pair.  The "woodneedle" case (needles with wood
properties, no wound, uniform flow) measures model + discretisation error.

Wound-width identifiability: for one case per layout the simulation is
repeated with w + 0.02 cm; dy/dw is appended as an extra Jacobian column to
the analytic model at the fitted point and the Bayesian CRB of w is reported
(1 pulse per heater, 5 mK at 25 Hz), along with how much of dy/dw the
analytic parameters can absorb.

Output: results/needle_cost.json, results/needle_cost_wound_id.json,
fig_needle_flow.png, fig_needle_cost.png
"""

from __future__ import annotations

import json
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
import pandas as pd
import scipy.sparse as sp
from scipy.sparse.linalg import splu, spsolve

import vecmodel as vm

HERE = Path(__file__).resolve().parent
OUT = HERE / "results"

DX = 0.02
HX0, HX1, Y0, Y1 = -2.0, 3.0, -2.0, 2.0  # heat domain
DX0, DX1 = -12.0, 13.0  # Darcy domain in x (same y)
C_WOOD = 2.8
KL = 0.0025
R_ANISO = 1.85
R_NEEDLE = 0.065
C_NEEDLE = 3.0
LAM_NEEDLE = 0.02
DT = 0.1
T_END = 180.0
E_PER_LEN = vm.A_REF * 4 * np.pi * C_WOOD  # J/cm, matches the analytic amplitude
SIG_PT = vm.SIGMA_25HZ / np.sqrt(12.5)
T_OUT = np.arange(1.5, T_END + 1e-9, 0.5)


def needle_positions(layout_name, beta_deg):
    lay = vm.layouts()[layout_name]
    b = np.deg2rad(beta_deg)
    Rm = np.array([[np.cos(b), -np.sin(b)], [np.sin(b), np.cos(b)]])
    return lay, lay.needles @ Rm.T


def darcy(centres, a_block, Kp, U):
    """Face fluxes (cm/h) on the Darcy grid.  Returns qx (nx+1, ny), qy (nx, ny+1)."""
    xs = np.arange(DX0 + DX / 2, DX1, DX)
    ys = np.arange(Y0 + DX / 2, Y1, DX)
    nx, ny = len(xs), len(ys)
    X, Y = np.meshgrid(xs, ys, indexing="ij")
    kx = np.full((nx, ny), float(Kp))
    ky = np.ones((nx, ny))
    for c in centres:
        m = np.hypot(X - c[0], Y - c[1]) <= a_block
        kx[m] = ky[m] = 1e-9
    idx = np.arange(nx * ny).reshape(nx, ny)
    gx = 2 * kx[:-1] * kx[1:] / (kx[:-1] + kx[1:])
    gy = 2 * ky[:, :-1] * ky[:, 1:] / (ky[:, :-1] + ky[:, 1:])
    rows, cols, vals = [], [], []
    diag = np.zeros(nx * ny)
    rhs = np.zeros(nx * ny)
    for a, c_, g in ((idx[:-1].ravel(), idx[1:].ravel(), gx.ravel()), (idx[:, :-1].ravel(), idx[:, 1:].ravel(), gy.ravel())):
        rows += [a, c_]; cols += [c_, a]; vals += [g, g]
        np.add.at(diag, a, -g); np.add.at(diag, c_, -g)
    G = U / Kp  # |dp/dx| far from the probe, so q = U there
    for i, xf in ((0, DX0), (nx - 1, DX1)):
        g = 2 * kx[i]  # half-cell to the boundary face
        diag[idx[i]] -= g
        rhs[idx[i]] -= g * (-G * xf)
    A = sp.csr_matrix((np.concatenate(vals), (np.concatenate(rows), np.concatenate(cols))), shape=(nx * ny,) * 2) + sp.diags(diag)
    p = spsolve(A.tocsc(), rhs).reshape(nx, ny)
    qx = np.zeros((nx + 1, ny))
    qx[1:-1] = -gx * (p[1:] - p[:-1]) / DX
    qx[0] = -2 * kx[0] * (p[0] - (-G * DX0)) / DX
    qx[-1] = -2 * kx[-1] * ((-G * DX1) - p[-1]) / DX
    qy = np.zeros((nx, ny + 1))
    qy[:, 1:-1] = -gy * (p[:, 1:] - p[:, :-1]) / DX
    return xs, ys, qx, qy


def crop_heat(xs, qx, qy):
    i0 = int(round((HX0 - DX0) / DX))
    n = int(round((HX1 - HX0) / DX))
    return qx[i0:i0 + n + 1], qy[i0:i0 + n]


def simulate(layout_name, beta_deg, U, w, Kp, needle_kind="steel"):
    """Thermistor curves for every heater/thermistor pair in Model.pairs order."""
    lay, pos = needle_positions(layout_name, beta_deg)
    xs = np.arange(HX0 + DX / 2, HX1, DX)
    ys = np.arange(Y0 + DX / 2, Y1, DX)
    nx, ny = len(xs), len(ys)
    X, Y = np.meshgrid(xs, ys, indexing="ij")
    lamx = np.full((nx, ny), KL * C_WOOD)
    lamy = np.full((nx, ny), KL / R_ANISO * C_WOOD)
    C = np.full((nx, ny), C_WOOD)
    masks = []
    for p in pos:
        d = np.hypot(X - p[0], Y - p[1])
        if needle_kind == "steel":
            if w > 2 * R_NEEDLE:
                m = d <= w / 2
                lamx[m] = lamy[m] = KL / R_ANISO * C_WOOD
            nd = d <= R_NEEDLE
            lamx[nd] = lamy[nd] = LAM_NEEDLE
            C[nd] = C_NEEDLE
        masks.append(d <= R_NEEDLE)
    if needle_kind == "steel":
        _, _, qxD, qyD = darcy(pos, max(w / 2, R_NEEDLE), Kp, U)
        qx, qy = crop_heat(None, qxD, qyD)
    else:  # validation: uniform flow, needles made of wood
        qx = np.full((nx + 1, ny), float(U))
        qy = np.zeros((nx, ny + 1))
    qx, qy = qx / 3600.0, qy / 3600.0

    N = nx * ny
    idx = np.arange(N).reshape(nx, ny)
    rows, cols, vals = [], [], []
    diag = np.zeros(N)
    gx = 2 * lamx[:-1] * lamx[1:] / (lamx[:-1] + lamx[1:]) / DX**2
    gy = 2 * lamy[:, :-1] * lamy[:, 1:] / (lamy[:, :-1] + lamy[:, 1:]) / DX**2
    for a, c_, g in ((idx[:-1].ravel(), idx[1:].ravel(), gx.ravel()), (idx[:, :-1].ravel(), idx[:, 1:].ravel(), gy.ravel())):
        rows += [a, c_]; cols += [c_, a]; vals += [g, g]
        np.add.at(diag, a, -g); np.add.at(diag, c_, -g)
    for sl, lam in ((np.s_[0, :], lamx), (np.s_[-1, :], lamx), (np.s_[:, 0], lamy), (np.s_[:, -1], lamy)):
        np.add.at(diag, idx[sl].ravel(), -2 * lam[sl].ravel() / DX**2)
    # conservative central advection: face flux F = C q (T_a + T_c)/2
    for a, c_, q in ((idx[:-1].ravel(), idx[1:].ravel(), qx[1:-1].ravel()), (idx[:, :-1].ravel(), idx[:, 1:].ravel(), qy[:, 1:-1].ravel())):
        f = C_WOOD * q / (2 * DX)
        rows += [a, a, c_, c_]; cols += [a, c_, a, c_]; vals += [-f, -f, f, f]
    Lop = sp.csr_matrix((np.concatenate(vals), (np.concatenate(rows), np.concatenate(cols))), shape=(N, N)) + sp.diags(diag)
    Cv = C.ravel()
    lu = splu((sp.diags(Cv / DT) - 0.5 * Lop).tocsc())
    B = (sp.diags(Cv / DT) + 0.5 * Lop).tocsr()

    h_ids, t_ids = lay.heater_ids, lay.therm_ids
    curves = {}
    nsteps = int(round(T_END / DT))
    for k, hn in enumerate(h_ids):
        hm = masks[hn].ravel()
        src = np.zeros(N)
        src[hm] = E_PER_LEN / vm.DUR / (hm.sum() * DX**2)
        T = np.zeros(N)
        rec = np.zeros((len(t_ids), len(T_OUT)))
        ko = 0
        for n in range(nsteps):
            T = lu.solve(B @ T + (src if n * DT < vm.DUR - 1e-9 else 0.0))
            tn = (n + 1) * DT
            if ko < len(T_OUT) and abs(tn - T_OUT[ko]) < 1e-6:
                Tg = T.reshape(nx, ny)
                for j, tn_ in enumerate(t_ids):
                    rec[j, ko] = Tg[masks[tn_]].mean()
                ko += 1
        curves[k] = rec
    return np.concatenate([curves[k][j] for k, j in lay.pairs])


def model_for(layout_name, kind):
    lay = vm.layouts()[layout_name]
    return vm.Model(lay, aligned=(kind == "aligned"), baseline=False, t_pre=np.array([]), blur=True, loss=True)


def fit_vec(layout_name, phi_true, U, y, kind, fixed=False):
    fm = model_for(layout_name, kind)
    starts = []
    for th0 in (0.0, 60.0, 120.0):
        r = np.deg2rad(th0)
        if kind == "aligned":
            starts += [fm.theta0(th=r, ux=3 * np.cos(r), uy=3 * np.sin(r)), fm.theta0(th=r, ux=-3 * np.cos(r), uy=-3 * np.sin(r))]
        else:
            starts.append(fm.theta0(kT=KL / 1.5, th=r))
    sp_, sg = (1e-4, 1e-4) if fixed else (0.03, 0.02)
    p, cost = vm.fit(fm, y, SIG_PT, {"dx": sp_, "dy": sp_, "lg": sg}, starts)
    d = fm.derived(p)
    resid = fm.predict(p) - y
    return p, dict(phi_hat=d["phi_deg"], err_phi=float(vm.wrap180(d["phi_deg"] - phi_true)),
                   speed_ratio=d["speed"] / U, kL_hat=max(d["kL"], d["kT"]), kT_hat=min(d["kL"], d["kT"]),
                   c_hat=float(np.exp(p[fm.idx["lc"]])),
                   rmse_mK=float(1e3 * np.sqrt(np.mean(resid**2))))


def fit_1d(y, U):
    """The project's 1-D estimator (fit_4x4_pulse.fit_pair) on the two curves."""
    sys.path.insert(0, str(HERE.parent))
    sys.path.insert(0, str(HERE.parent / "compression_research_20261024"))
    import fit_4x4_pulse as fit
    from common import fit_args
    yy = y.reshape(2, -1)
    fr = pd.DataFrame({"time_s": T_OUT, "t1_rise_c": yy[0], "t2_rise_c": yy[1]})
    res, _ = fit.fit_pair(fr, "x", 1, 2, vm.DUR, fit_args())
    return dict(err_phi=np.nan, speed_ratio=res.nominal_velocity_cm_hr / U, kL_hat=res.nominal_k_cm2_s,
                rmse_mK=1e3 * res.trace_rmse_c)


def case(args):
    layout_name, beta, U, wk, Kp = args
    if wk == "woodneedle":
        y = simulate(layout_name, beta, U, 0.0, 1.0, needle_kind="wood")
        w = 0.0
    else:
        w = float(wk)
        y = simulate(layout_name, beta, U, w, Kp)
    base = dict(layout=layout_name, n_needles=vm.layouts()[layout_name].n_needles, beta=beta, phi_true=-beta,
                U=U, wound=wk, Kp=Kp if wk != "woodneedle" else np.nan)
    if layout_name == "std_axial":
        return [dict(**base, model="1d_fit_pair", **fit_1d(y, U))]
    rows = []
    for kind in ("general", "aligned"):
        for fixed in (True, False):
            _, r = fit_vec(layout_name, -beta, U, y, kind, fixed)
            rows.append(dict(**base, model=kind, positions="fixed" if fixed else "prior", **r))
    return rows


def wound_id(args):
    """CRB on wound width w from the curves, with all analytic parameters free."""
    layout_name, beta, U, w, Kp = args
    dw = 0.02
    y0 = simulate(layout_name, beta, U, w, Kp)
    y1 = simulate(layout_name, beta, U, w + dw, Kp)
    dydw = (y1 - y0) / dw
    p, r = fit_vec(layout_name, -beta, U, y0, "general", fixed=True)
    fm = model_for(layout_name, "general")
    J = fm.jacobian(p)
    Ja = np.column_stack((J, dydw))
    F = Ja.T @ Ja / SIG_PT**2
    P = np.zeros_like(F)
    P[:-1, :-1] = fm.prior_precision(0.03, 0.02)
    Fp = F + P
    sd_w = float(np.sqrt(np.linalg.inv(Fp)[-1, -1]))
    sd_w_alone = float(1 / np.sqrt(F[-1, -1]))
    # fraction of the wound signature the analytic model absorbs
    coef, *_ = np.linalg.lstsq(J, dydw, rcond=None)
    absorbed = 1 - float(np.sum((dydw - J @ coef) ** 2) / np.sum(dydw**2))
    return dict(layout=layout_name, beta=beta, U=U, w=w, Kp=Kp, sd_w_cm=sd_w, sd_w_if_only_unknown=sd_w_alone,
                frac_signature_absorbed=absorbed,
                max_dT_mK_per_0p1cm=float(1e3 * 0.1 * np.abs(dydw).max()),
                fit_rmse_mK=r["rmse_mK"], speed_ratio=r["speed_ratio"])


LAYOUTS = ("std_axial", "T3_axial+side", "tri3", "Y3", "hex6", "2H_tri", "dual3", "dual4")


def jobs():
    J = []
    for lay in LAYOUTS:
        for beta in (0.0, 20.0):
            J.append((lay, beta, 10.0, "woodneedle", 1.0))
            for Kp in (1.0, 30.0, 300.0, 3000.0):
                J.append((lay, beta, 10.0, "0.2", Kp))
            for wk in ("0.13", "0.3"):
                J.append((lay, beta, 10.0, wk, 300.0))
            J.append((lay, beta, 2.0, "0.2", 300.0))
    return J


def flow_figure():
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    fig, axs = plt.subplots(2, 3, figsize=(13, 5.4), constrained_layout=True)
    for row, lay in enumerate(("std_axial", "Y3")):
        _, pos = needle_positions(lay, 0.0)
        for col, Kp in enumerate((1.0, 30.0, 300.0)):
            xs, ys, qx, qy = darcy(pos, 0.1, Kp, 1.0)
            ux = 0.5 * (qx[1:] + qx[:-1])
            sel = (xs > -2.5) & (xs < 2.5)
            ax = axs[row, col]
            im = ax.pcolormesh(xs[sel], ys, ux[sel].T, vmin=0, vmax=1.6, cmap="viridis", shading="auto")
            for p in pos:
                ax.add_patch(plt.Circle(p, 0.1, fc="w", ec="k", lw=0.6))
            ax.set_xlim(-2.5, 2.5); ax.set_ylim(-1.0, 1.4); ax.set_aspect("equal")
            ax.set_title(f"{lay}, Kp = {Kp:g}")
            ax.set_xlabel("axial x (cm), flow +x")
            if col == 0:
                ax.set_ylabel("tangential y (cm)")
    fig.colorbar(im, ax=axs, label="axial flux / far-field", shrink=0.8)
    fig.suptitle("Darcy flow around needle + wound (radius 0.1 cm): the stagnant strip grows as a*sqrt(Kp)")
    fig.savefig(HERE / "fig_needle_flow.png", dpi=130)


def plot_results(df):
    """Physics bias (geometry fixed at truth) vs permeability anisotropy."""
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    d = df[(df.wound == "0.2") & (df.U == 10.0) & ((df.positions == "fixed") | (df.model == "1d_fit_pair"))]
    d = d[d.model.isin(["general", "1d_fit_pair"])]
    fig, axs = plt.subplots(1, 3, figsize=(14, 4.2), constrained_layout=True)
    cols = {n: f"C{i}" for i, n in enumerate(LAYOUTS)}
    for lay in LAYOUTS:
        if lay == "T3_axial+side":
            continue  # general model not identifiable at beta = 0 (fit diverges); see table
        for beta, ls in ((0.0, "--"), (20.0, "-")):
            s = d[(d.layout == lay) & (d.beta == beta)].sort_values("Kp")
            if s.empty:
                continue
            lab = f"{lay} ({s.n_needles.iloc[0]})" if beta == 20.0 else None
            axs[0].plot(s.Kp, s.speed_ratio, "o" + ls, color=cols[lay], ms=3, label=lab)
            if lay != "std_axial":
                axs[1].plot(s.Kp, s.err_phi, "o" + ls, color=cols[lay], ms=3)
            axs[2].plot(s.Kp, s.rmse_mK, "o" + ls, color=cols[lay], ms=3)
    for ax in axs:
        ax.set_xscale("log")
        ax.set_xlabel("Kp = axial / tangential permeability")
        ax.grid(alpha=0.3, which="both")
    axs[0].set_ylabel("fitted speed / true far-field speed")
    axs[1].set_ylabel("flow-angle error (deg)")
    axs[2].set_ylabel("fit RMSE (mK); noise is 1.4 mK per 0.5 s point")
    axs[0].legend(fontsize=7, title="layout (needles)", title_fontsize=7)
    fig.suptitle("FD truth with 2 mm wounds, 10 cm/h; geometry fixed at truth; dashed = probe vertical, solid = rotated 20 deg")
    fig.savefig(HERE / "fig_needle_cost.png", dpi=130)


def main():
    flow_figure()
    J = jobs()
    with ProcessPoolExecutor(max_workers=16) as ex:
        res = [r for rr in ex.map(case, J) for r in rr]
        wid = list(ex.map(wound_id, [(lay, 0.0, 10.0, 0.2, Kp) for lay in LAYOUTS if lay != "std_axial" for Kp in (1.0, 300.0)]))
    df = pd.DataFrame(res)
    df.to_json(OUT / "needle_cost.json", orient="records", indent=1)
    (OUT / "needle_cost_wound_id.json").write_text(json.dumps(wid, indent=1))
    plot_results(df)
    pd.set_option("display.width", 220)
    print(df.round(3).to_string(index=False))
    print(pd.DataFrame(wid).round(4).to_string(index=False))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "quick":
        import time
        t = time.time()
        for a in (("Y3", 0.0, 10.0, "woodneedle", 1.0), ("Y3", 0.0, 10.0, "0.2", 300.0), ("std_axial", 0.0, 10.0, "0.2", 300.0)):
            print(pd.DataFrame(case(a)).round(4).to_string(), time.time() - t)
    elif len(sys.argv) > 1 and sys.argv[1] == "fig":
        flow_figure()
        plot_results(pd.read_json(OUT / "needle_cost.json", dtype={"wound": str}))
    else:
        main()
