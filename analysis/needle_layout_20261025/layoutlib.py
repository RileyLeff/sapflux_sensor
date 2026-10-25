"""Shared geometry and fit helpers for the needle-layout studies.

Fits reuse vecmodel.Model / line_response unchanged; the only addition is a
MAP wrapper (a copy of vecmodel.fit) that can also pin the anisotropy ratio
kL/kT, which the "axis" model needs (flow along the probe axis, R known:
what a two-needle pair can support).
"""

from __future__ import annotations

import numpy as np
from scipy.optimize import least_squares

import fd2d as F
import needle_cost as nc
import vecmodel as vm

S = 0.6  # cm axial spacing
R_H = 0.045  # heater needle radius (0.9 mm OD)
R_S = 0.041  # 21G sensor needle radius (0.82 mm OD)
R_E30 = nc.R_NEEDLE  # East30 needle radius (1.3 mm OD)
WOUND_ADD = 0.035  # cm per side (2 mm wound for the 1.3 mm needle)
WOUND_RATIO = 2.0 / 1.3  # proportional-wound sensitivity case
SIG_PT = vm.SIGMA_25HZ / np.sqrt(12.5)
KL = F.KL
R_ANISO = F.R_ANISO


def wound_r(r, mode):
    return r + WOUND_ADD if mode == "std" else r * WOUND_RATIO


def a_heater(mode="std"):
    return wound_r(R_H, mode)


def rot(pos, beta_deg):
    b = np.deg2rad(beta_deg)
    Rm = np.array([[np.cos(b), -np.sin(b)], [np.sin(b), np.cos(b)]])
    return np.asarray(pos, float) @ Rm.T


def study_a_layout(kind, off):
    """Probe-frame (heater, sensors, r_heater, r_sensor)."""
    if kind == "east30":
        return np.zeros(2), np.array([[S, 0.0], [-S, 0.0]]), R_E30, R_E30
    if kind == "pair":  # point-symmetric pair: up at +off, down at -off
        return np.zeros(2), np.array([[S, off], [-S, -off]]), R_H, R_S
    if kind == "y3":  # lowercase y: two up at +-off, one down at -off
        return np.zeros(2), np.array([[S, off], [S, -off], [-S, -off]]), R_H, R_S
    raise KeyError(kind)


def fd_needles(heater, sensors, rh, rs, wound_mode, beta):
    pos = rot(np.vstack((heater, sensors)), beta)
    rr = [rh] + [rs] * len(sensors)
    if wound_mode == "none":
        return [F.Needle(p[0], p[1], r, 0.0) for p, r in zip(pos, rr)]
    if wound_mode == "e30":  # East30 2 mm wound on every needle
        return [F.Needle(p[0], p[1], r, 0.1) for p, r in zip(pos, rr)]
    return [F.Needle(p[0], p[1], r, wound_r(r, wound_mode)) for p, r in zip(pos, rr)]


def vm_layout(heater, sensors, name="custom"):
    return vm.Layout(name, np.atleast_2d(heater), np.asarray(sensors, float))


def map_fit(model, y, prior_sd, starts, R_fixed=None, max_nfev=400, sigma_pt=SIG_PT):
    """vecmodel.fit plus an optional hard (1e-4) constraint log(kL/kT) = log R_fixed."""
    n = len(model.names)
    sel, sd = [], []
    for name, i in model.idx.items():
        for pref, s in prior_sd.items():
            if name.startswith(pref):
                sel.append(i); sd.append(s)
                break
    sel, sd = np.array(sel, int), np.array(sd, float)
    iL, iT = model.idx["lkL"], model.idx["lkT"]

    def resid(p):
        r = (model.predict(p) - y) / sigma_pt
        extra = [] if R_fixed is None else [(p[iL] - p[iT] - np.log(R_fixed)) / 1e-4]
        return np.concatenate((r, p[sel] / sd, extra))

    best = None
    for p0 in starts:
        try:
            res = least_squares(resid, p0, method="trf", x_scale="jac", max_nfev=max_nfev)
        except Exception:
            continue
        if best is None or res.cost < best.cost:
            best = res
    return best.x, best.cost


def fit_layout(lay, y, kind, phi_true, U, c0, pos_sd=1e-4, A0=None):
    """Geometry fixed at truth (positions/gains 1e-4), blur + loss free.

    kind: 'general' (flow may cross grain), 'aligned' (flow along grain,
    theta free), 'axis' (flow along the probe x axis, R = 1.85 known);
    suffix '_R' pins R = kL/kT = 1.85 in the general/aligned models."""
    Rfix = kind == "axis" or kind.endswith("_R")
    base = kind.removesuffix("_R")
    fm = vm.Model(lay, aligned=(base != "general"), baseline=False, t_pre=np.array([]), blur=True, loss=True)
    starts = []
    # R-pinned fits get a denser start set: the y's likelihood surface has
    # local minima ~20 deg off when rotated (seen with 3 starts).
    ths = (-40.0, -20.0, 0.0, 20.0, 40.0, 60.0, 90.0, 120.0) if Rfix and kind != "axis" else (0.0, 60.0, 120.0)
    if base == "general":
        for th0 in ths:
            r = np.deg2rad(th0)
            for s_ in ((0,) if not Rfix else (1, -1)):
                p = fm.theta0(kT=KL / (R_ANISO if Rfix else 1.5), th=r, ux=3 * s_ * np.cos(r), uy=3 * s_ * np.sin(r),
                              A=F.A_NEW if A0 is None else A0)
                p[fm.idx["lc"]] = np.log(c0)
                starts.append(p)
    elif base == "aligned":
        for th0 in ths:
            r = np.deg2rad(th0)
            for s_ in (1, -1):
                p = fm.theta0(th=r, ux=3 * s_ * np.cos(r), uy=3 * s_ * np.sin(r), A=F.A_NEW if A0 is None else A0)
                p[fm.idx["lc"]] = np.log(c0)
                starts.append(p)
    else:
        for s_ in (1, -1):
            p = fm.theta0(kT=KL / R_ANISO, th=0.0, ux=3 * s_, A=F.A_NEW if A0 is None else A0)
            p[fm.idx["lc"]] = np.log(c0)
            starts.append(p)
    prior = {"dx": pos_sd, "dy": pos_sd, "lg": 1e-4}
    if kind == "axis":
        prior["th"] = 1e-4
    p, _ = map_fit(fm, y, prior, starts, R_fixed=R_ANISO if Rfix else None)
    d = fm.derived(p)
    resid = fm.predict(p) - y
    if kind == "axis":
        U_ = p[fm.idx["U"]]
        err_phi = np.nan
        spd = U_  # signed: along +x
    else:
        err_phi = float(vm.wrap180(d["phi_deg"] - phi_true))
        spd = d["speed"]
    return dict(speed=float(spd), speed_ratio=float(spd / U) if U else np.nan, err_phi=err_phi, kL_hat=d["kL"], kT_hat=d["kT"],
                c_hat=float(np.exp(p[fm.idx["lc"]])), rmse_mK=float(1e3 * np.sqrt(np.mean(resid**2))))


def hrm(y_down, y_up, x_axial, k=KL, t0=60.0, t1=80.0):
    """Heat-ratio method on two curves sampled at fd2d.T_OUT: V = k/x ln(Td/Tu) (cm/h)."""
    m = (F.T_OUT >= t0) & (F.T_OUT <= t1)
    return float(3600.0 * k / x_axial * np.log(y_down[m].mean() / y_up[m].mean()))
