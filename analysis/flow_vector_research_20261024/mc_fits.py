"""Monte Carlo whole-curve fits: do real nonlinear fits reach the bounds,
and what do the confounders do?

Each trial draws needle displacements (sd sig_pos per coordinate, every
needle), channel gains (sd sig_gain, log), a random linear baseline drift
per channel and optional curvature, adds white noise (5 mK per 25 Hz sample,
represented on a 0.5 s grid as sigma/sqrt(12.5)), detrends each channel
with a line fitted to -55..-1 s (as the project's pipeline does), and fits
the post-pulse curves of every heater/thermistor pair jointly with the
MAP whole-curve fit in vecmodel.fit (nominal positions, Gaussian priors on
displacements and gains, multistart).

Scenarios
  base      truth = model (flow along grain, R = 1.85)
  iso       fit assumes isotropic K (as penta-needle soil probes do)
  offgrain  flow 20 deg off the grain axis, fitted with the aligned model
  curv      unmodelled baseline curvature, uniform +-2 mK/min^2
  badprior  true position sd 0.06 cm, prior still says 0.03 cm
  gain5     channel gain sd 5 % (prior still 2 %)

Output: results/mc_*.json, printed tables.
"""

from __future__ import annotations

import json
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
import pandas as pd

import vecmodel as vm

HERE = Path(__file__).resolve().parent
OUT = HERE / "results"
KL = 0.0025
SIG_PT = vm.SIGMA_25HZ / np.sqrt(12.5)


def one_trial(cfg):
    lay = vm.layouts(cfg.get("s", vm.S_REF))[cfg["layout"]]
    rng = np.random.default_rng(cfg["seed"])
    R = cfg.get("R", 1.85)
    grain = np.deg2rad(cfg["phi"] - cfg.get("offgrain", 0.0))
    phi = np.deg2rad(cfg["phi"])
    U = cfg["U"]
    sig_pos_true = cfg.get("sig_pos_true", 0.03)
    sig_gain_true = cfg.get("sig_gain_true", 0.02)

    truth = vm.Model(lay, aligned=False, baseline=False)
    p = truth.theta0(kL=KL, kT=KL / R, th=grain, ux=U * np.cos(phi), uy=U * np.sin(phi))
    for n, i in truth.idx.items():
        if n.startswith(("dx", "dy")):
            p[i] = rng.normal(0, sig_pos_true)
        elif n.startswith("lg"):
            p[i] = rng.normal(0, sig_gain_true)
    y = truth.predict(p).reshape(len(truth.pairs), -1)
    t = truth.t
    curv = cfg.get("curv", 0.0)  # mK/min^2 range
    ys = []
    for row in y:
        drift = rng.normal(0, 0.02) + rng.uniform(-40, 40) / 60e3 * t
        if curv:
            drift = drift + rng.uniform(-curv, curv) / 3.6e6 * t**2 / 2.0  # mK/min^2 -> degC/s^2
        obs = row + drift + rng.normal(0, SIG_PT, len(t))
        pre = (t >= -55) & (t <= -1)
        c = np.polyfit(t[pre], obs[pre], 1)
        ys.append((obs - np.polyval(c, t))[truth.n_pre:])
    yobs = np.concatenate(ys)

    aligned = cfg["model"] == "aligned"
    fm = vm.Model(lay, aligned=aligned, baseline=False, t_pre=np.array([]))
    starts = []
    for th0 in (0.0, 60.0, 120.0):
        if aligned:
            for U0 in (3.0, -3.0):
                starts.append(fm.theta0(th=np.deg2rad(th0), ux=U0 * np.cos(np.deg2rad(th0)), uy=U0 * np.sin(np.deg2rad(th0))))
        else:
            starts.append(fm.theta0(kL=KL, kT=KL / 1.5, th=np.deg2rad(th0)))
    prior = {"dx": 0.03, "dy": 0.03, "lg": 0.02}
    ph, cost = vm.fit(fm, yobs, SIG_PT, prior, starts, iso=cfg.get("iso", False))
    d = fm.derived(ph)
    return dict(
        **{k: v for k, v in cfg.items()},
        phi_hat=d["phi_deg"], speed_hat=d["speed"], kL_hat=d["kL"], kT_hat=d["kT"],
        grain_hat=d["grain_deg"], cost=float(cost), n_data=len(yobs),
        err_phi=float(vm.wrap180(d["phi_deg"] - cfg["phi"])),
        err_speed_rel=float(d["speed"] / U - 1.0),
    )


def crb_for(cfg):
    lay = vm.layouts(cfg.get("s", vm.S_REF))[cfg["layout"]]
    aligned = cfg["model"] == "aligned"
    m = vm.Model(lay, aligned=aligned, baseline=False, t_pre=np.array([]))
    ph = np.deg2rad(cfg["phi"])
    p = m.theta0(kL=KL, kT=KL / cfg.get("R", 1.85), th=ph, ux=cfg["U"] * np.cos(ph), uy=cfg["U"] * np.sin(ph))
    return vm.crb(m, p, m.prior_precision(0.03, 0.02))


def summarize(df, keys):
    rows = []
    for k, g in df.groupby(keys):
        e = g.err_phi.to_numpy()
        es = g.err_speed_rel.to_numpy()
        cfg = g.iloc[0].to_dict()
        cr = crb_for(cfg) if cfg.get("scenario") == "base" else {}
        rows.append(dict(zip(keys, k if isinstance(k, tuple) else (k,)), n=len(g),
                         phi_bias=float(np.mean(e)), phi_rmse=float(np.sqrt(np.mean(e**2))),
                         phi_p95=float(np.percentile(np.abs(e), 95)), frac_gt20=float(np.mean(np.abs(e) > 20)),
                         speed_bias=float(np.mean(es)), speed_rmse=float(np.sqrt(np.mean(es**2))),
                         crb_phi=cr.get("phi", np.nan), crb_speed=cr.get("speed_rel", np.nan)))
    return pd.DataFrame(rows)


def main(n_trials=40):
    cfgs = []
    seed = 1000
    for layout in ("tri3", "hex6", "2H_tri", "T3_axial+side", "cross4"):
        for model in ("general", "aligned"):
            for phi in (0.0, 25.0, 90.0):
                for U in (2.0, 10.0):
                    for _ in range(n_trials):
                        seed += 1
                        cfgs.append(dict(scenario="base", layout=layout, model=model, phi=phi, U=U, seed=seed))
    for layout in ("tri3", "hex6", "T3_axial+side", "2H_tri"):
        for scen, extra, model in (
            ("iso", dict(iso=True), "general"),
            ("offgrain", dict(offgrain=20.0), "aligned"),
            ("offgrain_general", dict(offgrain=20.0), "general"),
            ("curv", dict(curv=2.0), "general"),
            ("badprior", dict(sig_pos_true=0.06), "general"),
            ("gain5", dict(sig_gain_true=0.05), "general"),
        ):
            for U in (2.0, 10.0):
                for _ in range(n_trials):
                    seed += 1
                    cfgs.append(dict(scenario=scen, layout=layout, model=model, phi=25.0, U=U, seed=seed, **extra))
    with ProcessPoolExecutor(max_workers=16) as ex:
        res = list(ex.map(one_trial, cfgs, chunksize=4))
    df = pd.DataFrame(res)
    df.to_json(OUT / "mc_trials.json", orient="records")
    pd.set_option("display.width", 200)
    base = summarize(df[df.scenario == "base"], ["layout", "model", "phi", "U"])
    base["scenario"] = "base"
    print(base.round(3).to_string(index=False))
    conf = summarize(df[df.scenario != "base"], ["scenario", "layout", "model", "U"])
    print(conf.round(3).to_string(index=False))
    base.to_json(OUT / "mc_base_summary.json", orient="records", indent=1)
    conf.to_json(OUT / "mc_confounder_summary.json", orient="records", indent=1)


if __name__ == "__main__":
    main(int(sys.argv[1]) if len(sys.argv) > 1 else 40)
