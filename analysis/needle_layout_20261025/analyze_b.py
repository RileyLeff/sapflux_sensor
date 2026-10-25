"""Study B tables and figures from the results/ files written by study_b.py.

Criterion everywhere: |V_hat(z) - V(z)| <= max(0.5 cm/h, 5 % of V(z)), at
every thermistor depth (bark and heartwood included: true V = 0 there).

1. Deterministic drift (analytic truth, 40 depths): for each estimator and
   drift kind, the largest drift angle (of 0.25 / 0.5 / 1 / 2 deg) at which
   every depth passes ("pass up to"), and the worst error/threshold at 1 deg.
2. Per-depth angle tolerance from the displacement sweep:
   theta*(z) = atan(delta*(V(z)) / z), delta* the per-depth displacement
   tolerance at that depth's V; the minimum over the sapwood is the spec.
3. FD truth (wounds, Kp 300, 20 depths): every estimator is first given
   one linear wound-calibration factor (slope of V_hat on V at the nominal
   geometry over the sapwood), as an installed probe would be, then scored
   with the same criterion.
4. Structured fits and the CRB / Monte Carlo summaries.

Output: results/study_b_summary.json, fig_b_profiles.png, fig_b_tolerance.png
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
OUT = HERE / "results"
sys.path.insert(0, str(HERE))
import study_b as B  # noqa: E402

PER_DEPTH = ("hrm", "axis_nom", "fit_pair", "fit_pair_amp", "axis_free")
ZONE_SAP = (B.Z >= B.BARK) & (B.Z < B.HW)


def thr(v):
    return np.maximum(0.5, 0.05 * np.abs(v))


def det_scores(df, st, cal=None):
    """Worst |err|/thr over depths, per (kind, ang, ent, vpk, method)."""
    rows = []
    for (k, a, e, vpk), g in df.groupby(["kind", "ang", "ent_mm", "vpk"]):
        g = g.sort_values("z")
        v = g.v.to_numpy()
        for m in PER_DEPTH:
            V = g[m].to_numpy() / (cal[m] if cal else 1.0)
            r = np.abs(V - v) / thr(v)
            i = int(np.argmax(r))
            rows.append(dict(kind=k, ang=a, ent_mm=e, vpk=vpk, method=m, worst=float(r[i]), z_worst=float(g.z.iloc[i]),
                             abs_err_worst=float(abs(V[i] - v[i]))))
        for s in st:
            if (s["kind"], s["ang"], s["ent_mm"], s["vpk"]) != (k, a, e, vpk):
                continue
            V = np.array(s["V"]) / (cal.get(s["method"], 1.0) if cal else 1.0)
            r = np.abs(V - v) / thr(v)
            i = int(np.argmax(r))
            rows.append(dict(kind=k, ang=a, ent_mm=e, vpk=vpk, method=s["method"], worst=float(r[i]),
                             z_worst=float(g.z.iloc[i]), abs_err_worst=float(abs(V[i] - v[i])), rmse_mK=s["rmse_mK"]))
    return pd.DataFrame(rows)


def pass_up_to(sc):
    """Largest tested angle a such that every angle <= a passes (worst <= 1)."""
    rows = []
    d = sc[sc.ent_mm == 0]
    for (k, vpk, m), g in d[d.kind != "nominal"].groupby(["kind", "vpk", "method"]):
        g = g.sort_values("ang")
        nom = d[(d.kind == "nominal") & (d.vpk == vpk) & (d.method == m)].worst
        ok_nom = bool(len(nom) and nom.iloc[0] <= 1.0)
        best = 0.0
        if ok_nom:
            for a, w in zip(g.ang, g.worst):
                if w <= 1.0:
                    best = a
                else:
                    break
        w1 = g[g.ang == 1.0].worst
        rows.append(dict(kind=k, vpk=vpk, method=m, nominal_ok=ok_nom, pass_up_to_deg=best,
                         worst_at_1deg=float(w1.iloc[0]) if len(w1) else np.nan))
    return pd.DataFrame(rows)


def theta_star(tol, vpk):
    """Per-depth angle tolerance (deg) from the displacement sweep."""
    v = B.vprofile(B.Z, vpk)
    out = []
    for (typ, m), g in tol.groupby(["type", "method"]):
        g = g.copy()
        g["tol_mm"] = g.tol_mm.fillna(np.inf)
        dmin = g.groupby("v").tol_mm.min()  # worse of the two signs
        vv, dd = dmin.index.to_numpy(float), dmin.to_numpy(float)
        dd = np.where(np.isinf(dd), 50.0, dd)  # "passes to 3.5 mm" -> large
        dz = np.interp(np.abs(v), vv, dd)
        th = np.degrees(np.arctan(dz / 10.0 / B.Z))
        out.append(dict(type=typ, method=m, vpk=vpk, theta_deg=th.tolist(), delta_mm=dz.tolist(),
                        theta_min_sapwood=float(th[ZONE_SAP].min()), z_min_sapwood=float(B.Z[ZONE_SAP][np.argmin(th[ZONE_SAP])]),
                        theta_min_all=float(th.min()), z_min_all=float(B.Z[np.argmin(th)])))
    return out


def fd_calibration(fd, stf):
    nom = fd[(fd.kind == "nominal") & (fd.z >= B.BARK) & (fd.z < B.HW)]
    cal = {}
    for m in PER_DEPTH:
        cal[m] = float(np.sum(nom[m] * nom.v) / np.sum(nom.v**2))
    for s in stf:
        if s["kind"] == "nominal":
            z = np.array(B.Z_FD)
            sel = (z >= B.BARK) & (z < B.HW)
            V = np.array(s["V"])[sel]
            v = B.vprofile(z, s["vpk"])[sel]
            cal[s["method"]] = float(np.sum(V * v) / np.sum(v**2))
    return cal


def main():
    pd.set_option("display.width", 250)
    summ = {}
    da = pd.read_json(OUT / "study_b_det_analytic.json")
    sa = json.loads((OUT / "study_b_struct_analytic.json").read_text())
    sc = det_scores(da, sa)
    pu = pass_up_to(sc)
    summ["analytic_pass_up_to"] = pu.to_dict("records")
    summ["analytic_scores"] = sc.to_dict("records")
    print("== analytic truth: largest drift angle (deg) passing at every depth ==")
    print(pu.pivot_table(index=["vpk", "kind"], columns="method", values="pass_up_to_deg").to_string())
    print("== worst error/threshold at 1 deg ==")
    print(pu.pivot_table(index=["vpk", "kind"], columns="method", values="worst_at_1deg").round(2).to_string())
    ent = sc[sc.ent_mm > 0].pivot_table(index=["vpk", "kind", "ent_mm"], columns="method", values="worst")
    print("== entry offsets (mm): worst error/threshold ==")
    print(ent.round(2).to_string())

    tol = pd.read_json(OUT / "study_b_sweep_tol.json")
    ts = theta_star(tol, 15.0) + theta_star(tol, 3.0)
    summ["theta_star"] = ts
    print("== per-depth angle tolerance from the sweep (deg, min over sapwood) ==")
    print(pd.DataFrame(ts).pivot_table(index=["vpk", "type"], columns="method", values="theta_min_sapwood").round(3).to_string())

    if (OUT / "study_b_det_fd.json").exists():
        fd = pd.read_json(OUT / "study_b_det_fd.json")
        sf = json.loads((OUT / "study_b_struct_fd.json").read_text())
        cal = fd_calibration(fd, sf)
        summ["fd_wound_calibration"] = cal
        print("== FD wound calibration factors (V_hat/V at nominal, sapwood) ==", {k: round(v, 3) for k, v in cal.items()})
        # score on the FD depth grid
        Zsave = B.Z
        scf = det_scores(fd, sf, cal)
        puf = pass_up_to(scf)
        summ["fd_pass_up_to"] = puf.to_dict("records")
        summ["fd_scores"] = scf.to_dict("records")
        print("== FD truth (wound-calibrated): largest angle passing at every depth ==")
        print(puf.pivot_table(index=["vpk", "kind"], columns="method", values="pass_up_to_deg").to_string())
        print(puf.pivot_table(index=["vpk", "kind"], columns="method", values="worst_at_1deg").round(2).to_string())
        nomf = scf[scf.kind == "nominal"][["method", "worst", "z_worst", "abs_err_worst"]]
        print("== FD nominal after calibration ==")
        print(nomf.round(3).to_string(index=False))
        summ["fd_nominal"] = nomf.to_dict("records")
        B.Z = Zsave

    if (OUT / "study_b_struct_crb.json").exists():
        crb = json.loads((OUT / "study_b_struct_crb.json").read_text())
        summ["struct_crb"] = [{k: v for k, v in r.items() if not isinstance(v, list)} | dict(
            sd_V_max=float(np.max(r["sd_V"])), sd_V_median=float(np.median(r["sd_V"]))) for r in crb]
        print("== structured-fit CRB (1 pulse) ==")
        print(pd.DataFrame(summ["struct_crb"]).round(4).to_string(index=False))
    if (OUT / "study_b_mc.json").exists():
        mc = pd.read_json(OUT / "study_b_mc.json")
        s = mc.groupby(["method", "vpk", "sig_deg"]).max_err_over_thr.agg(
            pass_frac=lambda x: float(np.mean(x <= 1.0)), p50="median", p90=lambda x: float(np.percentile(x, 90))).reset_index()
        summ["mc"] = s.to_dict("records")
        print("== Monte Carlo: random drift sd per hole (deg), entry sd 0.1 mm ==")
        print(s.pivot_table(index=["vpk", "sig_deg"], columns="method", values="pass_frac").round(2).to_string())
        print(s.pivot_table(index=["vpk", "sig_deg"], columns="method", values="p90").round(2).to_string())

    (OUT / "study_b_summary.json").write_text(json.dumps(summ, indent=1, default=float))
    plots(da, sa, ts)


def plots(da, sa, ts):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    fig, axs = plt.subplots(1, 3, figsize=(15, 4.4), constrained_layout=True)
    for ax, (kind, ang) in zip(axs, (("dn_toward", 1.0), ("both_toward", 1.0), ("dn_side", 2.0))):
        g = da[(da.kind == kind) & (da.ang == ang) & (da.vpk == 15.0) & (da.ent_mm == 0)].sort_values("z")
        ax.fill_between(g.z, g.v - thr(g.v), g.v + thr(g.v), color="0.85", label="pass band")
        ax.plot(g.z, g.v, "k-", lw=1.5, label="true V(z)")
        for m, c in (("hrm", "C3"), ("fit_pair", "C0"), ("fit_pair_amp", "C2")):
            ax.plot(g.z, g[m], ".-", color=c, ms=3, lw=1, label=m)
        for s in sa:
            if (s["kind"], s["ang"], s["vpk"], s["ent_mm"]) == (kind, ang, 15.0, 0.0) and s["method"] in ("struct_x", "struct_xy"):
                ax.plot(B.Z, s["V"], "--", lw=1.2, label=s["method"])
        ax.set(xlabel="depth below bark surface (cm)", ylabel="V (cm/h)", title=f"{kind} {ang:g} deg (analytic truth)",
               ylim=(-3, 22))
        ax.grid(alpha=0.3)
    axs[0].legend(fontsize=8)
    fig.savefig(HERE / "fig_b_profiles.png", dpi=120)

    fig, axs = plt.subplots(1, 3, figsize=(15, 4.2), constrained_layout=True)
    for ax, typ in zip(axs, ("diff_axial", "common_axial", "dn_side")):
        for r in ts:
            if r["type"] == typ and r["vpk"] == 15.0:
                ax.semilogy(B.Z, np.clip(r["theta_deg"], 1e-3, 30), ".-", ms=3, label=r["method"])
        ax.axvspan(0, B.BARK, color="0.9"); ax.axvspan(B.HW, 12, color="0.9")
        ax.set(xlabel="depth below bark surface (cm)", ylabel="angle tolerance theta*(z) (deg)",
               title=f"{typ}: day profile (peak 15 cm/h)", ylim=(1e-2, 30))
        ax.grid(alpha=0.3, which="both")
    axs[0].legend(fontsize=8)
    fig.savefig(HERE / "fig_b_tolerance.png", dpi=120)


if __name__ == "__main__":
    main()
