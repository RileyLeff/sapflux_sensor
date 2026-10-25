"""Sanity check on the one good bench capture (Aug 24, soaked 4x4, 0.99 s
pulse): how well does the vector model (here in its 1-D geometry, since the
probe has only up/down needles) fit real curves, and how big is the real
systematic misfit compared with the signatures direction finding relies on?

For each pair (middle, head) the detrended rises are averaged into 0.5 s
bins and fitted with:
  fit_pair   the project's 1-D estimator (reference)
  vec        vector model, isotropic K, u along x, needle x-offsets with
             prior 0.03 cm, gains prior 2 %, amplitude free
  vec+blur   same plus the finite-needle blur c free
  vec+uy     same as vec+blur plus a cross-flow component uy (identifiable
             only through u^2, i.e. |uy|)
  vec+blur+loss  vec+blur plus a common-mode loss rate lam (heat leaving
             the plane: finite heater, needle conduction, block faces)

Output: results/real_capture.json
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "compression_research_20261024"))
import fit_4x4_pulse as fit  # noqa: E402
import vecmodel as vm  # noqa: E402
import common as C  # noqa: E402

w = C.real_pulses()[0]
temps = C.window_temps(w)
frame = pd.DataFrame({"time_s": w.t})
noise = {}
for c in w.live:
    frame[f"t{c}_rise_c"] = C.detrend(w.t, temps[c])
    pre = (w.t > -55) & (w.t < -1)
    noise[c] = float(np.std(np.diff(frame[f"t{c}_rise_c"][pre])) / np.sqrt(2))
args = C.fit_args()
edges = np.arange(1.25, 180.26, 0.5)
tb = 0.5 * (edges[:-1] + edges[1:])
out = {"pulse_s": w.pulse_s, "noise_25hz_mK": {k: 1e3 * v for k, v in noise.items()}}
lay = vm.Layout("aug_pair", np.zeros((1, 2)), np.array([[0.6, 0.0], [-0.6, 0.0]]))
for pair, (d, u) in w.pairs.items():
    res, _ = fit.fit_pair(frame, pair, d, u, w.pulse_s, args)
    ys = []
    for ch in (d, u):
        v = frame[f"t{ch}_rise_c"].to_numpy()
        idx = np.digitize(w.t, edges) - 1
        ok = (idx >= 0) & (idx < len(tb))
        ys.append(np.bincount(idx[ok], v[ok], len(tb)) / np.bincount(idx[ok], minlength=len(tb)))
    y = np.concatenate(ys)
    sig = np.mean([noise[d], noise[u]]) / np.sqrt(12.5)
    row = {"fit_pair": dict(v=res.nominal_velocity_cm_hr, k=res.nominal_k_cm2_s, eps=res.epsilon,
                            rmse_mK=1e3 * res.trace_rmse_c)}
    for label, blur, uyfree, loss in (("vec", False, False, False), ("vec+blur", True, False, False),
                                      ("vec+uy", True, True, False), ("vec+blur+loss", True, False, True)):
        m = vm.Model(lay, aligned=False, baseline=False, t_pre=np.array([]), t_post=tb, blur=blur, dur=w.pulse_s,
                     loss=loss)
        starts = []
        for uy0 in ((0.0, 3.0) if uyfree else (0.0,)):
            p0 = m.theta0(ux=0.0, uy=uy0, A=vm.A_REF)
            starts.append(p0)
        prior = {"dx": 0.03, "dy": 1e-4, "lg": 0.02, "th": 1e-4}
        if not uyfree:
            prior["uy"] = 1e-4
        p, cost = vm.fit(m, y, sig, prior, starts, iso=True)
        r = m.predict(p) - y
        dd = m.derived(p)
        row[label] = dict(ux=float(p[m.idx["ux"]]), uy=float(p[m.idx["uy"]]), k=dd["kL"],
                          c_cm2=float(np.exp(p[m.idx["lc"]])) if blur else 0.0,
                          dx_down=float(p[m.idx["dx1"]]), dx_up=float(p[m.idx["dx2"]]),
                          gain_down=float(p[m.idx["lg0"]]), gain_up=float(p[m.idx["lg1"]]),
                          lam_per_s=float(p[m.idx["lam0"]]) if loss else 0.0,
                          rmse_mK=float(1e3 * np.sqrt(np.mean(r**2))),
                          white_noise_floor_mK=float(1e3 * sig),
                          chi2_per_point=float(np.mean((r / sig) ** 2)))
    out[pair] = row
print(json.dumps(out, indent=1))
(HERE / "results/real_capture.json").write_text(json.dumps(out, indent=1))
