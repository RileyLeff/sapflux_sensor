"""Vector heat-pulse forward model, Fisher information and whole-curve fit.

Physics (derivation in README.md section 2).  Wood is treated locally as a
homogeneous anisotropic medium.  In the plane normal to the needles
(x = installer's "axial", y = tangential, z = radial along the needles) the
temperature rise from an infinitely long line heater that emits q W/m for
`dur` seconds is

    T(r, t) = A / sqrt(det K) * mean_{s in [0, min(t, dur)]}
              (1/tau) exp(-(r - u tau)^T K^-1 (r - u tau) / (4 tau)),
    tau = t - s,  A = q dur / (4 pi rho c)   [degC cm^2]

with K the 2x2 diffusivity tensor (principal values kL along the grain,
kT across it, grain at angle theta from the probe's x axis) and u the heat
velocity vector.  In the 1-D limit (r on the x axis, K isotropic, u along x)
this is exactly `fit_4x4_pulse.finite_pulse_response` divided by xbar^2,
which `check_model.py` verifies.

The model for a measurement is a set of *windows*, one per heater pulse.  In
window h every thermistor j records

    y_hj(t) = b0_hj + b1_hj t + g_j A_h S(p_j - p_h, t; K, u)

where g_j is the channel gain (thermistor calibration x needle contact),
A_h the heater amplitude, b0/b1 a linear baseline, and p the true needle
positions (nominal + unknown displacement).

Units: cm, s, degC; velocities in the parameter vector are cm/h.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np
from scipy.optimize import least_squares

GL_X, GL_W = np.polynomial.legendre.leggauss(16)

FS = 25.0  # Hz, per-channel sample rate on the board
SIGMA_25HZ = 0.005  # degC per 25 Hz sample (about 1.5 ADC counts)
DUR = 1.0  # s heat pulse
K_REF = 0.0025  # cm^2/s
S_REF = 0.6  # cm
PEAK_REF = 1.3  # degC peak rise at 0.6 cm, isotropic K_REF, no flow (bench)


def line_response(t, rx, ry, kL, kT, theta, ux, uy, dur=DUR):
    """S(r, t) in 1/cm^2 for one heater-thermistor vector r (cm).

    u in cm/s.  Mean over the pulse of the instantaneous moving
    line-source kernel (16-node Gauss-Legendre in source time)."""
    t = np.asarray(t, float)
    c, s = np.cos(theta), np.sin(theta)
    rl, rt = c * rx + s * ry, -s * rx + c * ry
    ul, ut = c * ux + s * uy, -s * ux + c * uy
    src = 0.5 * dur * (GL_X + 1.0)
    w = 0.5 * GL_W
    tau = t[:, None] - src[None, :]
    valid = tau > 0
    tau_ = np.where(valid, tau, 1.0)
    q = (rl - ul * tau_) ** 2 / kL + (rt - ut * tau_) ** 2 / kT
    val = np.where(valid, np.exp(-q / (4.0 * tau_)) / tau_, 0.0)
    return (val @ w) / np.sqrt(kL * kT)


def amplitude_ref() -> float:
    """A (degC cm^2) that gives PEAK_REF at S_REF for isotropic K_REF, no flow."""
    tt = np.linspace(1.0, 120.0, 4000)
    return PEAK_REF / line_response(tt, S_REF, 0.0, K_REF, K_REF, 0.0, 0.0, 0.0).max()


A_REF = amplitude_ref()


# --------------------------------------------------------------------------
# Layouts


@dataclass
class Layout:
    name: str
    heaters: np.ndarray  # (H, 2) cm
    therms: np.ndarray  # (M, 2) cm
    note: str = ""

    @property
    def n_needles(self) -> int:
        return len(self.heaters) + len(self.therms)


def ring(n: int, s: float, phase_deg: float = 0.0) -> np.ndarray:
    a = np.deg2rad(phase_deg + 360.0 * np.arange(n) / n)
    return np.column_stack((s * np.cos(a), s * np.sin(a)))


def layouts(s: float = S_REF) -> dict[str, Layout]:
    h0 = np.zeros((1, 2))
    r2 = s / np.sqrt(2.0)
    L = [
        Layout("std_axial", h0, np.array([[s, 0], [-s, 0]]),
               "East30 today: heater + up/down thermistor needles"),
        Layout("T3_axial+side", h0, np.array([[s, 0], [-s, 0], [0, s]]),
               "prior-encoding: up/down plus one tangential (Sapflow+-like)"),
        Layout("cross4", h0, np.array([[s, 0], [-s, 0], [0, s], [0, -s]]),
               "penta-needle: up/down/left/right"),
        Layout("tri3", h0, ring(3, s, 0.0), "3 needles at 120 deg, one on +x"),
        Layout("tri3_rot", h0, ring(3, s, 90.0), "3 needles at 120 deg, one on +y"),
        Layout("hex6", h0, ring(6, s, 0.0), "6 needles at 60 deg"),
        Layout("pent5", h0, ring(5, s, 0.0), "5 needles at 72 deg"),
        Layout("2H_diamond", np.array([[-r2, 0], [r2, 0]]), np.array([[0, r2], [0, -r2]]),
               "2 heaters on x, 2 thermistors on y; all H-T distances = s"),
        Layout("2H_line+side", np.array([[-s, 0], [s, 0]]), np.array([[0, 0], [0, s]]),
               "H-T-H on the axial line plus a side thermistor"),
        # Two heaters 1 s apart on x; three thermistors at the apexes of the
        # two equilateral triangles plus the far side, so every heater sees
        # thermistors in three directions at distance s or s*sqrt3.
        Layout("2H_tri", np.array([[-s / 2, 0], [s / 2, 0]]),
               np.array([[0, s * np.sqrt(3) / 2], [0, -s * np.sqrt(3) / 2], [s, s * np.sqrt(3) / 2]]),
               "2 heaters + 3 thermistors on a triangular lattice"),
    ]
    return {x.name: x for x in L}


# --------------------------------------------------------------------------
# Parameterised model


@dataclass
class Model:
    layout: Layout
    aligned: bool = False  # True: u is along the grain axis (speed U signed)
    t_pre: np.ndarray = field(default_factory=lambda: np.arange(-60.0, 0.0, 0.5))
    t_post: np.ndarray = field(default_factory=lambda: np.arange(1.5, 180.0 + 1e-9, 0.5))
    baseline: bool = True
    dur: float = DUR

    def __post_init__(self):
        H, M = len(self.layout.heaters), len(self.layout.therms)
        names = ["lkL", "lkT", "th"]
        names += ["U"] if self.aligned else ["ux", "uy"]
        names += [f"lA{h}" for h in range(H)]
        names += [f"lg{j}" for j in range(M)]
        for n in range(H + M):
            names += [f"dx{n}", f"dy{n}"]
        if self.baseline:
            for h in range(H):
                for j in range(M):
                    names += [f"b0_{h}_{j}", f"b1_{h}_{j}"]
        self.names = names
        self.idx = {n: i for i, n in enumerate(names)}
        self.H, self.M = H, M
        self.t = np.concatenate((self.t_pre, self.t_post))
        self.n_pre = len(self.t_pre)
        # samples represented by each grid point (white noise, 25 Hz)
        self.weight = FS * float(self.t[1] - self.t[0])

    def theta0(self, kL=K_REF, kT=K_REF, th=0.0, ux=0.0, uy=0.0, A=A_REF):
        p = np.zeros(len(self.names))
        p[self.idx["lkL"]] = np.log(kL)
        p[self.idx["lkT"]] = np.log(kT)
        p[self.idx["th"]] = th
        if self.aligned:
            p[self.idx["U"]] = ux * np.cos(th) + uy * np.sin(th)
        else:
            p[self.idx["ux"]], p[self.idx["uy"]] = ux, uy
        for h in range(self.H):
            p[self.idx[f"lA{h}"]] = np.log(A)
        return p

    def velocity(self, p):
        if self.aligned:
            U = p[self.idx["U"]]
            th = p[self.idx["th"]]
            return U * np.cos(th), U * np.sin(th)
        return p[self.idx["ux"]], p[self.idx["uy"]]

    def positions(self, p):
        H = self.H
        pos = np.vstack((self.layout.heaters, self.layout.therms)).astype(float).copy()
        for n in range(len(pos)):
            pos[n, 0] += p[self.idx[f"dx{n}"]]
            pos[n, 1] += p[self.idx[f"dy{n}"]]
        return pos[:H], pos[H:]

    def predict(self, p):
        kL, kT = np.exp(p[self.idx["lkL"]]), np.exp(p[self.idx["lkT"]])
        th = p[self.idx["th"]]
        ux, uy = self.velocity(p)
        ux, uy = ux / 3600.0, uy / 3600.0
        hp, tp = self.positions(p)
        out = []
        for h in range(self.H):
            A = np.exp(p[self.idx[f"lA{h}"]])
            for j in range(self.M):
                g = np.exp(p[self.idx[f"lg{j}"]])
                r = tp[j] - hp[h]
                y = np.zeros_like(self.t)
                y[self.n_pre:] = g * A * line_response(self.t_post, r[0], r[1], kL, kT, th, ux, uy, self.dur)
                if self.baseline:
                    y += p[self.idx[f"b0_{h}_{j}"]] + p[self.idx[f"b1_{h}_{j}"]] * self.t
                out.append(y)
        return np.concatenate(out)

    # ------------------------------------------------------------------
    def jacobian(self, p, which=None):
        which = range(len(p)) if which is None else which
        cols = []
        for i in which:
            h = 1e-5 if self.names[i][0] in "lt" or self.names[i] == "th" else 1e-4
            if self.names[i] in ("ux", "uy", "U"):
                h = 1e-3
            if self.names[i].startswith("b"):
                h = 1e-3
            dp = np.zeros_like(p)
            dp[i] = h
            cols.append((self.predict(p + dp) - self.predict(p - dp)) / (2 * h))
        return np.column_stack(cols)

    def fisher(self, p, sigma=SIGMA_25HZ, pulses_per_heater=1.0):
        J = self.jacobian(p)
        return J.T @ J * self.weight * pulses_per_heater / sigma**2

    def prior_precision(self, sig_pos=0.03, sig_gain=0.02, sig_lk=None, sig_lR=None):
        """Diagonal prior precision, plus optional log-k and log-anisotropy priors.

        Baselines get no prior (pre-pulse data pin them).  Heater amplitude
        gets no prior (rho c and effective heater length unknown)."""
        P = np.zeros((len(self.names), len(self.names)))
        for n, i in self.idx.items():
            if n.startswith(("dx", "dy")):
                P[i, i] = 1.0 / sig_pos**2
            elif n.startswith("lg"):
                P[i, i] = 1.0 / sig_gain**2
        if sig_lk is not None:
            for n in ("lkL", "lkT"):
                P[self.idx[n], self.idx[n]] += 1.0 / sig_lk**2
        if sig_lR is not None:  # prior on log(kL/kT)
            a, b = self.idx["lkL"], self.idx["lkT"]
            v = np.zeros(len(self.names))
            v[a], v[b] = 1.0, -1.0
            P += np.outer(v, v) / sig_lR**2
        return P

    def derived(self, p):
        """Flow angle (deg), speed (cm/h), kL, kT, grain angle (deg)."""
        ux, uy = self.velocity(p)
        return {
            "phi_deg": float(np.degrees(np.arctan2(uy, ux))),
            "speed": float(np.hypot(ux, uy)),
            "kL": float(np.exp(p[self.idx["lkL"]])),
            "kT": float(np.exp(p[self.idx["lkT"]])),
            "grain_deg": float(np.degrees(p[self.idx["th"]])),
        }

    def derived_grad(self, p):
        """Gradient rows of (phi rad, speed, log kL, log kT, theta rad) wrt p."""
        n = len(p)
        G = np.zeros((5, n))
        ux, uy = self.velocity(p)
        sp = np.hypot(ux, uy)
        if self.aligned:
            iU, ith = self.idx["U"], self.idx["th"]
            U = p[iU]
            G[0, ith] = 1.0
            G[1, iU] = np.sign(U) if U != 0 else 1.0
        else:
            ix, iy = self.idx["ux"], self.idx["uy"]
            G[0, ix], G[0, iy] = -uy / sp**2, ux / sp**2
            G[1, ix], G[1, iy] = ux / sp, uy / sp
        G[2, self.idx["lkL"]] = 1.0
        G[3, self.idx["lkT"]] = 1.0
        G[4, self.idx["th"]] = 1.0
        return G


def crb(model: Model, p, prior, pulses_per_heater=1.0, sigma=SIGMA_25HZ):
    """Posterior (Bayesian CRB) std of (phi deg, speed rel, kL rel, kT rel, theta deg)."""
    F = model.fisher(p, sigma, pulses_per_heater) + prior
    try:
        C = np.linalg.inv(F)
    except np.linalg.LinAlgError:
        return dict(phi=np.inf, speed_rel=np.inf, kL_rel=np.inf, kT_rel=np.inf, theta=np.inf, cond=np.inf)
    G = model.derived_grad(p)
    V = G @ C @ G.T
    sd = np.sqrt(np.clip(np.diag(V), 0, None))
    sp = model.derived(p)["speed"]
    return dict(
        phi=float(np.degrees(sd[0])), speed_rel=float(sd[1] / sp) if sp > 0 else np.inf,
        speed_abs=float(sd[1]), kL_rel=float(sd[2]), kT_rel=float(sd[3]),
        theta=float(np.degrees(sd[4])), cond=float(np.linalg.cond(F)),
    )


# --------------------------------------------------------------------------
# Whole-curve fit


def fit(model: Model, y, sigma_pt, prior_sd: dict, starts, prior_mean=None, max_nfev=400):
    """MAP fit: Gaussian likelihood on every curve sample + Gaussian priors.

    prior_sd maps parameter-name prefixes to prior std (around prior_mean,
    default 0).  `starts` is a list of initial parameter vectors; the best
    final cost wins.  Returns (p_hat, cost)."""
    n = len(model.names)
    mu = np.zeros(n) if prior_mean is None else prior_mean
    sel, sd = [], []
    for name, i in model.idx.items():
        for pref, s in prior_sd.items():
            if name.startswith(pref):
                sel.append(i)
                sd.append(s)
                break
    sel, sd = np.array(sel, int), np.array(sd, float)

    def resid(p):
        r = (model.predict(p) - y) / sigma_pt
        return np.concatenate((r, (p[sel] - mu[sel]) / sd))

    best = None
    for p0 in starts:
        try:
            res = least_squares(resid, p0, method="trf", x_scale="jac", max_nfev=max_nfev)
        except Exception:
            continue
        if best is None or res.cost < best.cost:
            best = res
    return best.x, best.cost


def wrap180(a):
    return (np.asarray(a) + 180.0) % 360.0 - 180.0


def wrap90(a):
    return (np.asarray(a) + 90.0) % 180.0 - 90.0
