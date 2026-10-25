"""Study B: drilling-drift tolerance for 12 cm flex-PCB needles.

Geometry.  A straight 0.9 mm heater line (the reference) and two 21G sensor
holes, downstream at x = +0.6 cm and upstream at x = -0.6 cm (flow +x, up the
stem), thermistors every 3 mm from z = 0.15 to 11.85 cm below the bark
surface.  Each sensor hole is a straight line with an entry offset e and a
drift angle theta relative to the heater:
    x(z) = x_nom + e_x + tan(theta_x) z,   y(z) = e_y + tan(theta_y) z,
so 1 deg moves the hole 1.75 mm at 10 cm.

Slices.  Each depth is an independent 2-D slice with its local sensor
positions.  For a straight infinite heater line in wood whose properties do
not change with depth, the temperature at a point is exactly the 2-D
solution at that point's in-plane offset (integrate the 3-D point kernel
along the line), whatever the tilt of the sensor hole; so the geometry part
of the slice approximation is exact.  What it neglects is depth variation of
V, k and rho c: heat reaching a thermistor has sampled the flow over a depth
window (depth_kernel.py: FWHM 3-5 mm at 20-60 s) and the deep heater end
(sec. B.1 of the README).

Wood (assumed): bark 0-0.5 cm, sapwood 0.5-8 cm, heartwood 8-12 cm.
    kL: bark 0.0018, sapwood 0.0026 -> 0.0024 (linear, drier inward),
        heartwood 0.0021 cm^2/s; R = kL/kT = 1.85 everywhere
    rho c: bark 2.0, sapwood 2.8, heartwood 2.3 J/cm^3/K
    V: 0 in bark and heartwood; in sapwood (d = z - 0.5)
        g = 1 - (1 - d/1.25)^2 for d <= 1.25, ((7.5 - d)/6.25)^1.5 beyond,
        V = Vpk g, Vpk = 15 cm/h (day) or 3 cm/h (low flow); peak at
        z = 1.75 cm, zero at the sapwood/heartwood boundary z = 8 cm.
Heater 4.8 J/cm, 1 s.

Truths.
  analytic  vecmodel.line_response with the needle blur c = (rh^2 + rs^2)/8,
            gains 1, no loss: the drift effect alone (all 40 depths).
  fd        fd2d (needle_cost physics) with 0.9 mm heater + 21G sensors and
            std wounds (needle + 0.35 mm per side), Kp = 300, area-fraction
            needles (aa = 4) so sub-cell drifts move the geometry
            continuously, dx = 0.02 cm (needle_cost's grid).  Subsampled to
            every other thermistor (20 depths) to keep runtime reasonable.

Per-depth estimators (two curves each).
  hrm        naive heat ratio, nominal 0.6 cm and k = 0.0025 (fit_pair's
             stock velocity, 60-80 s)
  axis_nom   vecmodel whole-curve fit, flow along x, R known, positions FIXED
             at nominal (blur free, loss free): the naive fixed-spacing fit
  fit_pair   the project's derivative/Marshall estimator
             (fit_4x4_pulse.fit_pair): alpha(t) = zeta - eps tau/t_eff
             resolves the up/down spacing asymmetry eps from the data; mean
             spacing at nominal.  This is the repo's "probe position from the
             trace" method (README top level; fit_4x4_pulse docstring).
  fit_pair_amp  fit_pair with the mean spacing from the amplitude,
             xbar = R^(1/4) sqrt(E / (4 pi rho c A)), rho c and R known
             (best case: needs heater energy, rho c, R and gains)
  axis_free  vecmodel whole-curve fit with every needle displacement free
             under a 0.03 cm prior: the repo's free-geometry fit (sec. 3/6 of
             the flow_vector README)

Structured estimator (struct_*): one joint fit of all depths: the line
parameters of both holes (entry + slope, x and optionally y), the blur, and
per depth kL, amplitude and V, with V = 0 fixed in heartwood (anchors).
Variants in STRUCT below.

Error metric: |V_hat(z) - V(z)| <= max(0.5 cm/h, 5 % V(z)) at every depth.
For the FD (wound) truth each estimator first gets one linear wound
correction factor fitted over the sapwood depths at nominal geometry (field
practice: one correction per wound width), and the error is measured after
it.

Output: results/study_b_*.json, fig_b_*.png
"""

from __future__ import annotations

import json
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
import pandas as pd
import scipy.sparse as sps
from scipy.optimize import least_squares

import fd2d as F
import layoutlib as L
import vecmodel as vm

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import fit_4x4_pulse as fit  # noqa: E402
from common import fit_args  # noqa: E402

HERE = Path(__file__).resolve().parent
OUT = HERE / "results"
WORKERS = 8  # run under `nice -n 15`

Z = 0.15 + 0.3 * np.arange(40)  # thermistor depths, cm
Z_FD = Z[::2]
BARK, HW = 0.5, 8.0
C0 = (L.R_H**2 + L.R_S**2) / 8.0
GRID_B = F.Grid(dx=0.02)
AA = 4
T = F.T_OUT


def zone(z):
    return np.where(z < BARK, 0, np.where(z < HW, 1, 2))


def wood_at(z):
    zn = int(zone(np.array(z)))
    if zn == 0:
        return 0.0018, 2.0
    if zn == 2:
        return 0.0021, 2.3
    return 0.0026 + (0.0024 - 0.0026) * (z - BARK) / (HW - BARK), 2.8


def vprofile(z, vpk):
    z = np.asarray(z, float)
    d = z - BARK
    g = np.where(d <= 1.25, 1 - (1 - d / 1.25) ** 2, ((7.5 - d) / 6.25).clip(0) ** 1.5)
    return np.where((z >= BARK) & (z < HW), vpk * g, 0.0)


def thresh(v):
    return np.maximum(0.5, 0.05 * np.abs(v))


# ---------------------------------------------------------------------------
# Drift cases: per hole (entry x, slope x, entry y, slope y); "dn" is the
# downstream hole at +0.6, "up" the upstream hole at -0.6.  "toward" means
# toward the heater.

def case_lines(kind, ang_deg, ent_mm=0.0):
    t = np.tan(np.deg2rad(ang_deg))
    e = ent_mm / 10.0
    z4 = np.zeros(4)
    dn, up = z4.copy(), z4.copy()
    if kind == "nominal":
        pass
    elif kind == "dn_toward":
        dn[1] = -t
    elif kind == "dn_away":
        dn[1] = +t
    elif kind == "up_toward":
        up[1] = +t
    elif kind == "up_away":
        up[1] = -t
    elif kind == "both_toward":  # converging holes: common-mode spacing loss
        dn[1], up[1] = -t, +t
    elif kind == "both_away":
        dn[1], up[1] = +t, -t
    elif kind == "parallel":  # both lean +x: same as the heater leaning -x (differential)
        dn[1], up[1] = +t, +t
    elif kind == "dn_side":
        dn[3] = t
    elif kind == "both_side_same":  # same as the heater leaning sideways
        dn[3], up[3] = t, t
    elif kind == "both_side_opp":
        dn[3], up[3] = t, -t
    elif kind == "dn_entry_toward":
        dn[0] = -e
    elif kind == "both_entry_toward":
        dn[0], up[0] = -e, +e
    elif kind == "dn_entry_side":
        dn[2] = e
    else:
        raise KeyError(kind)
    return dn, up


def positions(dn, up, z):
    """(x_d, y_d, x_u, y_u) at depth(s) z."""
    return (L.S + dn[0] + dn[1] * z, dn[2] + dn[3] * z, -L.S + up[0] + up[1] * z, up[2] + up[3] * z)


DET_KINDS = ("dn_toward", "dn_away", "up_toward", "up_away", "both_toward", "both_away", "parallel",
             "dn_side", "both_side_same", "both_side_opp")
FD_KINDS = ("dn_toward", "dn_away", "both_toward", "parallel", "dn_side", "both_side_same")
ANGLES = (0.25, 0.5, 1.0, 2.0)
ENTRY = (("dn_entry_toward", 0.2), ("dn_entry_toward", 0.5), ("both_entry_toward", 0.2),
         ("dn_entry_side", 0.5), ("dn_entry_side", 1.0))


def det_cases(kinds=DET_KINDS):
    C = [("nominal", 0.0, 0.0)]
    C += [(k, a, 0.0) for k in kinds for a in ANGLES]
    C += [(k, 0.0, e) for k, e in ENTRY]
    return C


# ---------------------------------------------------------------------------
# Truth curves


def analytic_curves(xd, yd, xu, yu, z, v):
    kL, rc = wood_at(z)
    A = F.E_NEW / (4 * np.pi * rc)
    out = []
    for x, y in ((xd, yd), (xu, yu)):
        out.append(A * vm.line_response(T, x, y, kL, kL / L.R_ANISO, 0.0, v / 3600.0, 0.0, F.DUR, C0))
    return np.array(out)


def fd_spec(xd, yd, xu, yu, z, v, Kp=300.0):
    kL, rc = wood_at(z)
    ndl = [F.Needle(0.0, 0.0, L.R_H, L.wound_r(L.R_H, "std")),
           F.Needle(xd, yd, L.R_S, L.wound_r(L.R_S, "std")),
           F.Needle(xu, yu, L.R_S, L.wound_r(L.R_S, "std"))]
    return dict(needles=ndl, heater=0, sensors=[1, 2], U=v, Kp=Kp, wood=F.Wood(kL, L.R_ANISO, rc), grid=GRID_B,
                E=F.E_NEW, aa=AA)


# ---------------------------------------------------------------------------
# Per-depth estimators

ARGS = fit_args()
LAY = L.vm_layout(np.zeros(2), np.array([[L.S, 0.0], [-L.S, 0.0]]))


def per_depth(y, with_vm=True):
    # 1e-9 relative tilt: fit_pair's alpha R^2 divides by the variance of
    # alpha, which is exactly zero for a symmetric zero-flow pair
    fr = pd.DataFrame({"time_s": T, "t1_rise_c": y[0] * (1 + 1e-9 * T / T[-1]), "t2_rise_c": y[1]})
    res, _ = fit.fit_pair(fr, "x", 1, 2, F.DUR, ARGS)
    xbar_amp = L.R_ANISO**0.25 * np.sqrt(F.E_NEW / (4 * np.pi * 2.8 * res.common_amplitude_c))
    out = dict(hrm=res.stock_velocity_cm_hr, fit_pair=res.nominal_velocity_cm_hr,
               eps=res.epsilon, tau=res.tau_s, zeta=res.alpha_intercept_zeta, xbar_amp=float(xbar_amp),
               amp=res.common_amplitude_c)
    out["fit_pair_amp_rc28"] = float(3600.0 * xbar_amp * res.alpha_intercept_zeta / res.tau_s)
    if with_vm:
        yy = y.ravel()
        out["axis_nom"] = L.fit_layout(LAY, yy, "axis", 0.0, 1.0, C0)["speed"]
        out["axis_free"] = L.fit_layout(LAY, yy, "axis", 0.0, 1.0, C0, pos_sd=0.03)["speed"]
    return out


def geom(kind, ang, ent, vpk, z):
    dn, up = case_lines(kind, ang, ent)
    xd, yd, xu, yu = positions(dn, up, z)
    return xd, yd, xu, yu, float(vprofile(z, vpk))


def job_depth(args):
    """args: (truth, kind, ang, ent, vpk, z, with_vm[, curves]).  FD curves come
    precomputed (Rust loop); analytic curves are evaluated here."""
    truth, kind, ang, ent, vpk, z, with_vm = args[:7]
    xd, yd, xu, yu, v = geom(kind, ang, ent, vpk, z)
    y = analytic_curves(xd, yd, xu, yu, z, v) if truth == "analytic" else args[7]
    r = per_depth(y, with_vm)
    # the amplitude estimator with the true rho c of the slice (fit_pair_amp)
    rc = wood_at(z)[1]
    r["fit_pair_amp"] = r["fit_pair_amp_rc28"] * np.sqrt(2.8 / rc)
    return dict(truth=truth, kind=kind, ang=ang, ent_mm=ent, vpk=vpk, z=float(z), v=v,
                dxd=float(xd - L.S), dyd=float(yd), dxu=float(xu + L.S), dyu=float(yu), **r), y


# ---------------------------------------------------------------------------
# Structured (joint line) fit


STRUCT = {
    # name: (fit y lines, entry prior sd cm, k per depth or per zone, heartwood anchored)
    "struct_x": (False, 1e-3, "depth", True),
    "struct_xy": (True, 1e-3, "depth", True),
    "struct_x_e01": (False, 0.01, "depth", True),
    "struct_x_kzone": (False, 1e-3, "zone", True),
    "struct_x_noanchor": (False, 1e-3, "depth", False),
}


class Struct:
    def __init__(self, z, ys, fit_y, sig_e, kmode, anchor, vstart):
        self.z, self.ys = np.asarray(z), ys
        self.n = len(z)
        self.fit_y, self.sig_e, self.kmode, self.anchor = fit_y, sig_e, kmode, anchor
        self.free_v = np.ones(self.n, bool) if not anchor else (np.asarray(z) < HW)
        nl = 8 if fit_y else 4
        self.nl = nl
        self.nk = self.n if kmode == "depth" else 3
        self.iv = np.nonzero(self.free_v)[0]
        self.sl_k = slice(nl + 1, nl + 1 + self.nk)
        self.sl_A = slice(self.sl_k.stop, self.sl_k.stop + self.n)
        self.sl_V = slice(self.sl_A.stop, self.sl_A.stop + len(self.iv))
        self.npar = self.sl_V.stop
        self.vstart = vstart

    def unpack(self, p):
        ln = p[:self.nl]
        if self.fit_y:
            dn = np.array([ln[0], ln[1], ln[2], ln[3]]); up = np.array([ln[4], ln[5], ln[6], ln[7]])
        else:
            dn = np.array([ln[0], ln[1], 0.0, 0.0]); up = np.array([ln[2], ln[3], 0.0, 0.0])
        c = np.exp(p[self.nl])
        lk = p[self.sl_k]
        kL = np.exp(lk) if self.kmode == "depth" else np.exp(lk[zone(self.z)])
        A = np.exp(p[self.sl_A])
        V = np.zeros(self.n)
        V[self.iv] = p[self.sl_V]
        return dn, up, c, kL, A, V

    def predict(self, p):
        dn, up, c, kL, A, V = self.unpack(p)
        xd, yd, xu, yu = positions(dn, up, self.z)
        out = np.empty((self.n, 2, len(T)))
        for j in range(self.n):
            for s, (x, y) in enumerate(((xd[j], yd[j]), (xu[j], yu[j]))):
                out[j, s] = A[j] * vm.line_response(T, x, y, kL[j], kL[j] / L.R_ANISO, 0.0, V[j] / 3600.0, 0.0, F.DUR, c)
        return out

    def resid(self, p):
        r = ((self.predict(p) - self.ys) / L.SIG_PT).ravel()
        ln = p[:self.nl]
        ent_idx = [0, 2, 4, 6] if self.fit_y else [0, 2]
        slp_idx = [1, 3, 5, 7] if self.fit_y else [1, 3]
        return np.concatenate((r, ln[ent_idx] / self.sig_e, ln[slp_idx] / 0.1))

    def sparsity(self):
        nr = self.n * 2 * len(T)
        nprior = (8 if self.fit_y else 4)
        S = sps.lil_matrix((nr + nprior, self.npar), dtype=int)
        S[:nr, :self.nl + 1] = 1
        blk = 2 * len(T)
        for j in range(self.n):
            rows = slice(j * blk, (j + 1) * blk)
            if self.kmode == "depth":
                S[rows, self.sl_k.start + j] = 1
            else:
                S[rows, self.sl_k.start + int(zone(self.z[j]))] = 1
            S[rows, self.sl_A.start + j] = 1
        for q, j in enumerate(self.iv):
            S[j * blk:(j + 1) * blk, self.sl_V.start + q] = 1
        for q in range(nprior):
            S[nr + q, q] = 1
        return S.tocsr()

    def p0(self):
        p = np.zeros(self.npar)
        if self.fit_y:
            p[3] = p[7] = 0.003  # break the y -> -y symmetry at zero offset
        p[self.nl] = np.log(C0)
        if self.kmode == "depth":
            p[self.sl_k] = np.log(0.0025)
        else:
            p[self.sl_k] = np.log(0.0025)
        rcs = np.array([wood_at(z)[1] for z in self.z])
        p[self.sl_A] = np.log(F.E_NEW / (4 * np.pi * 2.8)) + 0 * rcs
        p[self.sl_V] = self.vstart[self.iv]
        return p

    def fit(self):
        res = least_squares(self.resid, self.p0(), jac_sparsity=self.sparsity(), method="trf",
                            tr_solver="lsmr", x_scale="jac", max_nfev=200)
        return res


def struct_fit(args):
    name, z, ys, vstart, meta = args
    fit_y, sig_e, kmode, anchor = STRUCT[name]
    S = Struct(z, ys, fit_y, sig_e, kmode, anchor, vstart)
    res = S.fit()
    dn, up, c, kL, A, V = S.unpack(res.x)
    rm = float(1e3 * np.sqrt(np.mean((S.predict(res.x) - ys) ** 2)))
    return dict(**meta, method=name, V=V.tolist(), kL=kL.tolist(), dn=dn.tolist(), up=up.tolist(), c=float(c),
                rmse_mK=rm, nfev=int(res.nfev), cost=float(res.cost))


def struct_crb(vpk, name="struct_x"):
    """Bayesian CRB of the line slopes and V(z) for the structured fit at the
    nominal geometry, analytic truth, 1 pulse, 5 mK at 25 Hz."""
    fit_y, sig_e, kmode, anchor = STRUCT[name]
    v = vprofile(Z, vpk)
    ys = np.array([analytic_curves(L.S, 0, -L.S, 0, z, vv) for z, vv in zip(Z, v)])
    S = Struct(Z, ys, fit_y, sig_e, kmode, anchor, v)
    p = S.p0()
    p[S.sl_V] = v[S.iv]
    p[S.sl_A] = [np.log(F.E_NEW / (4 * np.pi * wood_at(z)[1])) for z in Z]
    if kmode == "depth":
        p[S.sl_k] = [np.log(wood_at(z)[0]) for z in Z]
    if fit_y:
        p[3] = p[7] = 1e-3  # y enters as y^2: evaluate just off zero
    # Jacobian by central differences of the residual vector (includes priors)
    J = np.zeros((len(S.resid(p)), S.npar))
    for i in range(S.npar):
        h = 1e-4 if i >= S.sl_V.start else 1e-5
        d = np.zeros_like(p); d[i] = h
        J[:, i] = (S.resid(p + d) - S.resid(p - d)) / (2 * h)
    Fm = J.T @ J
    Cv = np.linalg.pinv(Fm)
    sd = np.sqrt(np.clip(np.diag(Cv), 0, None))
    names = (["edx", "mdx", "edy", "mdy", "eux", "mux", "euy", "muy"] if fit_y else ["edx", "mdx", "eux", "mux"])
    out = dict(method=name, vpk=vpk, **{f"sd_{n}": float(s) for n, s in zip(names, sd[:S.nl])})
    # common-mode (mdx - mux) and differential (mdx + mux) slopes
    iD, iU = names.index("mdx"), names.index("mux")
    for lab, w in (("sd_slope_common_deg", (1, -1)), ("sd_slope_diff_deg", (1, 1))):
        g = np.zeros(S.npar); g[iD], g[iU] = 0.5 * w[0], 0.5 * w[1]
        out[lab] = float(np.degrees(np.sqrt(g @ Cv @ g)))
    out["sd_V"] = sd[S.sl_V].tolist()
    out["z_V"] = Z[S.iv].tolist()
    return out


# ---------------------------------------------------------------------------
# Displacement sweep: per-depth tolerance as a function of local displacement


SWEEP_TYPES = ("dn_axial", "common_axial", "diff_axial", "dn_side", "both_side")
SWEEP_D = np.round(np.concatenate((-np.geomspace(3.5, 0.02, 14), [0.0], np.geomspace(0.02, 3.5, 14))), 4)  # mm
SWEEP_V = (0.0, 0.5, 1.0, 2.0, 3.0, 6.0, 10.0, 15.0)


def sweep_job(args):
    typ, dmm, v = args
    d = dmm / 10.0
    xd, yd, xu, yu = L.S, 0.0, -L.S, 0.0
    if typ == "dn_axial":
        xd -= d  # positive d = toward the heater
    elif typ == "common_axial":
        xd -= d; xu += d
    elif typ == "diff_axial":
        xd -= d; xu -= d  # both shift -x: down closer, up farther
    elif typ == "dn_side":
        yd += d
    elif typ == "both_side":
        yd += d; yu += d
    zref = 2.0  # sapwood wood properties (kL 0.00253, rho c 2.8)
    y = analytic_curves(xd, yd, xu, yu, zref, v)
    r = per_depth(y, with_vm=True)
    r["fit_pair_amp"] = r["fit_pair_amp_rc28"]
    return dict(type=typ, d_mm=float(dmm), v=v, **r)


def tolerance_from_sweep(sw, method):
    """Smallest |d| (mm) at which |V_hat - V| > max(0.5, 5 %), per type, V and sign."""
    rows = []
    for (typ, v), g in sw.groupby(["type", "v"]):
        g = g.sort_values("d_mm")
        err = np.abs(g[method].to_numpy() - v)
        ok = err <= thresh(v)
        d = g.d_mm.to_numpy()
        for sgn in (1, -1):
            sel = (d * sgn >= 0)
            dd, oo, ee = np.abs(d[sel]), ok[sel], err[sel]
            o = np.argsort(dd); dd, oo, ee = dd[o], oo[o], ee[o]
            if not oo[0]:
                tol = 0.0
            elif oo.all():
                tol = np.inf
            else:
                k = int(np.argmax(~oo))
                # interpolate the crossing in error between the last pass and first fail
                t0, t1 = thresh(v), thresh(v)
                e0, e1 = ee[k - 1], ee[k]
                tol = float(dd[k - 1] + (dd[k] - dd[k - 1]) * (t0 - e0) / max(e1 - e0, 1e-12))
            rows.append(dict(type=typ, v=v, sign=sgn, method=method, tol_mm=tol, err_at_0=float(ee[0])))
    return pd.DataFrame(rows)


# ---------------------------------------------------------------------------
# Monte Carlo of random drift (analytic truth)


def mc_draw(args):
    seed, sig_deg, sig_e_mm, vpk = args
    rng = np.random.default_rng(seed)
    t = np.tan(np.deg2rad(rng.normal(0, sig_deg, 4)))
    e = rng.normal(0, sig_e_mm / 10.0, 4)
    dn = np.array([e[0], t[0], e[1], t[1]])
    up = np.array([e[2], t[2], e[3], t[3]])
    xd, yd, xu, yu = positions(dn, up, Z)
    v = vprofile(Z, vpk)
    ys = np.array([analytic_curves(xd[j], yd[j], xu[j], yu[j], z, v[j]) for j, z in enumerate(Z)])
    rows = [per_depth(y, with_vm=False) for y in ys]
    est = {m: np.array([r[m] for r in rows]) for m in ("hrm", "fit_pair", "fit_pair_amp_rc28")}
    rcs = np.array([wood_at(z)[1] for z in Z])
    est["fit_pair_amp"] = est.pop("fit_pair_amp_rc28") * np.sqrt(2.8 / rcs)
    meta = dict(seed=seed, sig_deg=sig_deg, sig_e_mm=sig_e_mm, vpk=vpk)
    out = []
    thr = thresh(v)
    for m, V in est.items():
        err = np.abs(V - v)
        out.append(dict(**meta, method=m, max_err_over_thr=float(np.max(err / thr)), max_abs_err=float(err.max())))
    for name in ("struct_x", "struct_xy"):
        r = struct_fit((name, Z, ys, est["fit_pair"], meta))
        V = np.array(r["V"])
        sel = Z < HW
        err = np.abs(V[sel] - v[sel])
        out.append(dict(**meta, method=name, max_err_over_thr=float(np.max(err / thr[sel])), max_abs_err=float(err.max())))
    return out


# ---------------------------------------------------------------------------


def run_det(truth, zs, kinds, vpks, with_vm):
    J = [(truth, k, a, e, vpk, z, with_vm) for (k, a, e) in det_cases(kinds) for vpk in vpks for z in zs]
    if truth == "fd":
        specs = [fd_spec(*geom(*j[1:5], j[5])[:4], j[5], geom(*j[1:5], j[5])[4]) for j in J]
        ys = F.simulate_many(specs, OUT / "_fdwork")
        J = [j + (y,) for j, y in zip(J, ys)]
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        res = list(ex.map(job_depth, J, chunksize=2))
    df = pd.DataFrame([r for r, _ in res])
    curves = {(j[1], j[2], j[3], j[4]): [] for j in J}
    for j, (_, y) in zip(J, res):
        curves[(j[1], j[2], j[3], j[4])].append(y)
    return df, {k: np.array(v) for k, v in curves.items()}


def run_struct(df, curves, zs, names, truth):
    J = []
    for (k, a, e, vpk), ys in curves.items():
        g = df[(df.kind == k) & (df.ang == a) & (df.ent_mm == e) & (df.vpk == vpk)].sort_values("z")
        for name in names:
            J.append((name, zs, ys, g.fit_pair.to_numpy(), dict(truth=truth, kind=k, ang=a, ent_mm=e, vpk=vpk)))
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        return list(ex.map(struct_fit, J))


def main(stage="all"):
    pd.set_option("display.width", 250)
    if stage in ("all", "sweep"):
        J = [(t, d, v) for t in SWEEP_TYPES for d in SWEEP_D for v in SWEEP_V]
        with ProcessPoolExecutor(max_workers=WORKERS) as ex:
            sw = pd.DataFrame(list(ex.map(sweep_job, J, chunksize=4)))
        sw.to_json(OUT / "study_b_sweep.json", orient="records", indent=1)
        tol = pd.concat([tolerance_from_sweep(sw, m) for m in ("hrm", "axis_nom", "fit_pair", "fit_pair_amp", "axis_free")])
        tol.to_json(OUT / "study_b_sweep_tol.json", orient="records", indent=1)
        print(tol.pivot_table(index=["type", "sign", "v"], columns="method", values="tol_mm").round(3).to_string())
    if stage in ("all", "analytic"):
        df, cv = run_det("analytic", Z, DET_KINDS, (15.0, 3.0), True)
        df.to_json(OUT / "study_b_det_analytic.json", orient="records")
        st = run_struct(df, cv, Z, list(STRUCT), "analytic")
        (OUT / "study_b_struct_analytic.json").write_text(json.dumps(st))
        np.savez_compressed(OUT / "study_b_curves_analytic.npz", **{"|".join(map(str, k)): v for k, v in cv.items()})
    if stage in ("all", "fd"):
        df, cv = run_det("fd", Z_FD, FD_KINDS, (15.0,), True)
        df.to_json(OUT / "study_b_det_fd.json", orient="records")
        st = run_struct(df, cv, Z_FD, ["struct_x", "struct_xy"], "fd")
        (OUT / "study_b_struct_fd.json").write_text(json.dumps(st))
        np.savez_compressed(OUT / "study_b_curves_fd.npz", **{"|".join(map(str, k)): v for k, v in cv.items()})
    if stage in ("all", "crb"):
        rows = []
        for vpk in (15.0, 3.0):
            for name in ("struct_x", "struct_xy", "struct_x_noanchor", "struct_x_kzone"):
                rows.append(struct_crb(vpk, name))
                print({k: v for k, v in rows[-1].items() if not isinstance(v, list)}, flush=True)
        (OUT / "study_b_struct_crb.json").write_text(json.dumps(rows, indent=1))
    if stage in ("all", "mc"):
        J = []
        seed = 5000
        for sig in (0.1, 0.25, 0.5, 1.0):
            for vpk in (15.0, 3.0):
                for _ in range(24):
                    seed += 1
                    J.append((seed, sig, 0.1, vpk))
        with ProcessPoolExecutor(max_workers=WORKERS) as ex:
            mc = pd.DataFrame([r for rr in ex.map(mc_draw, J) for r in rr])
        mc.to_json(OUT / "study_b_mc.json", orient="records")
        s = mc.groupby(["method", "vpk", "sig_deg"]).max_err_over_thr.agg(
            pass_frac=lambda x: float(np.mean(x <= 1.0)), p50="median", p90=lambda x: float(np.percentile(x, 90)))
        print(s.round(3).to_string())


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "all")
