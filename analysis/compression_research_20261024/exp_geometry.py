#!/usr/bin/env python3
"""Experiment: do SFK1's trained tables need per-geometry classes?

SFK1 (encoder_spec.py) codes the post-onset rise as 16 orthonormal spline
coefficients rotated by a KLT V, quantized at STEP = 3 counts and Rice coded
with a const per-index k-table plus a 2-bit per-channel offset.  V and the
k-table were trained on Marshall curves at the East30 0.6 cm axial spacing.
Here thermistors sit elsewhere relative to the heater (axial up/down at
0.5/0.6/0.75/1.0 cm, tangential and 45 deg diagonal at 0.6 cm), simulated
with an isotropic 2D moving finite line source in *physical* amplitude
(peak ~ 1/r^2, so far thermistors are smaller), and the coefficient bits are
compared under four table strategies:

  A   the shipped East30 tables (encoders.klt_rotation / rice_k_table)
  B   one rotation + k-table trained on the union of all geometries
  C   B's rotation, per-geometry k-table
  D   per-geometry rotation + k-table
plus a likely crate design and two controls:
  S   per-spacing class: rotation + k-table trained on axial up + down at the
      thermistor's distance from the heater, the way the East30 tables are
      (tangential/diagonal at 0.6 cm use the 0.6 cm class)
  A'  tables retrained by the same procedure on this simulator, East30 0.6 cm
      axial only (separates "simulator differs" from "geometry differs")
  Ak  shipped rotation, per-geometry k-table (k-table-only classes)

Training and test draws are independent.  Test windows are full 60 s + 181 s
records with baseline drift and 1.5-count white noise; the line is fitted
from the twelve 5 s Q3 baseline bins as SFK1 does.  Baseline bits do not
depend on the tables, so only the coefficient bits (incl. the 2-bit offset)
are compared.  Because V is a rotation and the step is the same in every
direction, reconstruction RMSE must not depend on the strategy; it is
reported as a check.

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/exp_geometry.py
"""

from __future__ import annotations

import numpy as np

import common as C
import encoder_spec as S
import encoders as E
import entropy as K
import fit_4x4_pulse as fit

OUT = C.HERE / "results"
FS = S.FS
STEP = S.STEP  # 3.0 counts
SIGMA = E.SIGMA_REF  # 1.5 counts
COUNTS_PER_C = 277.5  # as in encoders.klt_rotation
DUR = 1.0
N_TRAIN = 800  # per geometry (klt_rotation uses 800)
N_TEST = 500  # per geometry
OFFSETS = (-1, 0, 1, 2)

# name -> nominal (x, y) position in cm, x along the flow (downstream > 0)
_D = np.sqrt(0.5)
GEOMETRIES = {
    **{f"ax_dn_{d:g}": (d, 0.0) for d in (0.5, 0.6, 0.75, 1.0)},
    **{f"ax_up_{d:g}": (-d, 0.0) for d in (0.5, 0.6, 0.75, 1.0)},
    "tan_0.6": (0.0, 0.6),
    "diag45_dn_0.6": (0.6 * _D, 0.6 * _D),
    "diag45_up_0.6": (-0.6 * _D, 0.6 * _D),
}
EAST30 = ("ax_dn_0.6", "ax_up_0.6")

_GL_X, _GL_W = np.polynomial.legendre.leggauss(32)


def line_source(t: np.ndarray, x: float, y: float, k: float, v_cm_s: float, dur: float = DUR) -> np.ndarray:
    """(1/k) * int_0^dur exp(-|r - v e|^2 / (4 k e)) / e ds, e = t - s > 0.

    Isotropic 2D moving finite line source; the 1/k prefactor (heat q /
    (4 pi rho c k)) makes the v = 0 peak ~ dur / r^2, independent of k."""
    s = 0.5 * dur * (_GL_X + 1.0)
    wgt = 0.5 * dur * _GL_W
    e = np.asarray(t, float)[:, None] - s[None, :]
    ok = e > 0
    ee = np.where(ok, e, 1.0)
    d2 = (x - v_cm_s * ee) ** 2 + y**2
    val = np.where(ok, np.exp(-d2 / (4.0 * k * ee)) / ee, 0.0)
    return (val * wgt[None, :]).sum(1) / k


def draw_params(rng: np.random.Generator) -> dict:
    """Physics drawn as in encoders.klt_rotation, plus the amplitude scale and
    the whole-window drift of common.synth_signal_sets."""
    return {
        "k": rng.uniform(0.0016, 0.0034),
        "v_cm_h": rng.uniform(-4, 32),
        "eps": rng.uniform(-0.1, 0.1),  # radial spacing error (klt_rotation: +-10 %)
        "lat": rng.uniform(-0.03, 0.03),  # needle off its nominal line, cm
        "peak_ref": rng.uniform(280, 420),  # 0.6 cm, v = 0 peak, counts
        "drift_mK_min": rng.uniform(-40, 40),
        "curv_mK_min2": rng.uniform(-2, 2),
    }


_REF_T = np.linspace(0.5, 181, 1500)


def rise(tp: np.ndarray, geom: str, p: dict) -> np.ndarray:
    """Noise-free rise in counts at geometry `geom` (physical amplitude)."""
    x0, y0 = GEOMETRIES[geom]
    r0 = np.hypot(x0, y0)
    ux, uy = x0 / r0, y0 / r0  # radial unit vector; lateral = (-uy, ux)
    x = x0 * (1 + p["eps"]) - uy * p["lat"]
    y = y0 * (1 + p["eps"]) + ux * p["lat"]
    v = p["v_cm_h"] / 3600.0
    ref = line_source(_REF_T, 0.6, 0.0, p["k"], 0.0).max()
    return p["peak_ref"] / ref * line_source(tp, x, y, p["k"], v)


def nominal() -> tuple[tuple, np.ndarray, np.ndarray]:
    tp = S.nominal_post_t()
    key = tuple(np.round(tp, 4))
    return key, tp, E.spline_q(key, S.KNOTS, S.T0)


def train_coefs(geom: str, n: int, seed: int) -> np.ndarray:
    """Orthonormal spline coefficients Q^T (rise + post-onset curvature) of
    noise-free curves, the training input klt_rotation uses."""
    _, tp, Q = nominal()
    rng = np.random.default_rng(seed)
    X = []
    for _ in range(n):
        p = draw_params(rng)
        curv = p["curv_mK_min2"] * 1e-3 * COUNTS_PER_C * (tp / 60.0) ** 2 / 2
        X.append(Q.T @ (rise(tp, geom, p) + curv))
    return np.array(X)


def train_rotation(X: np.ndarray) -> np.ndarray:
    _, _, vt = np.linalg.svd(X, full_matrices=False)  # uncentered, as klt_rotation
    return vt.T


def train_k_table(X: np.ndarray, V: np.ndarray) -> tuple:
    """encoders.rice_k_table's rule on given training coefficients."""
    Xr = X @ V / STEP
    noise = SIGMA / STEP
    med = np.median(np.sqrt(Xr**2 + noise**2), axis=0)
    return tuple(int(v) for v in np.maximum(0, np.round(np.log2(med + 0.5))))


def test_set(geom: str, n: int, seed: int) -> dict:
    """Full 60 s + 181 s windows with drift and white noise; returns the
    line-removed post-onset rise, the noise-free post-onset signal relative
    to the fitted line, and peak counts."""
    _, tp, Q = nominal()
    rng = np.random.default_rng(seed)
    npre = 12 * S.FW_BIN
    tpre = (np.arange(npre) - npre) / FS
    tc = tpre.reshape(12, -1).mean(1)
    rises, clean, peaks = [], [], []
    for _ in range(n):
        p = draw_params(rng)
        tm_pre, tm_post = tpre / 60.0, tp / 60.0
        drift_pre = 1e-3 * COUNTS_PER_C * (p["drift_mK_min"] * tm_pre + 0.5 * p["curv_mK_min2"] * tm_pre**2)
        drift_post = 1e-3 * COUNTS_PER_C * (p["drift_mK_min"] * tm_post + 0.5 * p["curv_mK_min2"] * tm_post**2)
        r = rise(tp, geom, p)
        base = 8000.0
        y_pre = base + drift_pre + rng.normal(0, SIGMA, npre)
        y_post = base + drift_post + r + rng.normal(0, SIGMA, len(tp))
        b = np.round(8 * y_pre.reshape(12, -1).mean(1)) / 8  # Q3 bins, as SFK1
        bs, a = np.polyfit(tc, b, 1)
        line = a + bs * tp
        rises.append(y_post - line)
        clean.append(base + drift_post + r - line)
        peaks.append(r.max())
    return {"rise": np.array(rises), "clean": np.array(clean), "peak": np.array(peaks)}


def rice_bits(ci: np.ndarray, table: tuple) -> np.ndarray:
    """Vectorised encoders.rice_table_bits over rows of ci (N, 16)."""
    u = K.zigzag(ci)
    tab = np.array(table)
    best = None
    for o in OFFSETS:
        kk = np.maximum(0, tab + o)[None, :]
        q = u >> kk
        ln = np.where(q >= K.RICE_ESCAPE_Q, K.RICE_ESCAPE_Q + 20, q + 1 + kk).sum(1)
        best = ln if best is None else np.minimum(best, ln)
    return 2 + best


def evaluate(ts: dict, V: np.ndarray, table: tuple) -> dict:
    _, _, Q = nominal()
    QV = Q @ V
    c = ts["rise"] @ QV  # (N, 16) = V^T Q^T rise
    ci = np.round(c / STEP).astype(np.int64)
    bits = rice_bits(ci, table)
    recon = ci * STEP @ QV.T
    rmse = np.sqrt(np.mean((recon - ts["clean"]) ** 2, axis=1))
    proj = c @ QV.T  # unquantised: = Q Q^T rise for any rotation V
    return {"bits": bits, "rmse": rmse, "proj": proj}


def sanity() -> dict:
    """Unit checks: the 2D source reduces to fit_4x4_pulse's 1D model on the
    axis, and the vectorised Rice count equals encoders.rice_table_bits."""
    t = np.linspace(0.05, 181, 3000)
    k, v_h, d = 0.0025, 12.0, 0.6
    tau = d**2 / k
    zeta = v_h / 3600 * tau / d
    rel = []
    for side in (1, -1):
        mine = line_source(t, side * d, 0.0, k, v_h / 3600) * k  # drop the 1/k prefactor
        ref = fit.finite_pulse_response(t, zeta, tau, 0.0, side, DUR) / tau
        rel.append(float(np.max(np.abs(mine - ref)) / np.max(ref)))
    rng = np.random.default_rng(5)
    kt = S.k_table(S.KNOTS)
    ci = np.round(rng.laplace(0, 1, (200, 16)) * 2.0 ** np.array(kt)).astype(np.int64)
    ci[0, 7] = 10**5  # exercise the escape
    vec = rice_bits(ci, kt)
    ref_b = np.array([E.rice_table_bits(row, kt) for row in ci])
    return {"line_source_vs_finite_pulse_max_rel_err": max(rel),
            "rice_vectorised_mismatches": int(np.sum(vec != ref_b))}


def main() -> None:
    OUT.mkdir(exist_ok=True)
    key, _, Q = nominal()
    chk = sanity()
    print("sanity:", chk)
    assert chk["rice_vectorised_mismatches"] == 0 and chk["line_source_vs_finite_pulse_max_rel_err"] < 1e-9

    # shipped East30 tables
    V_A = E.klt_rotation(key, S.KNOTS, S.T0, DUR)
    K_A = S.k_table(S.KNOTS)
    assert tuple(K_A) == tuple(S.fw_k_table(S.KNOTS)), (K_A, S.fw_k_table(S.KNOTS))

    names = list(GEOMETRIES)
    Xtr = {g: train_coefs(g, N_TRAIN, 1000 + i) for i, g in enumerate(names)}

    X_east = np.vstack([Xtr[g] for g in EAST30])
    V_Ap = train_rotation(X_east)
    K_Ap = train_k_table(X_east, V_Ap)
    X_all = np.vstack([Xtr[g] for g in names])
    V_B = train_rotation(X_all)
    K_B = train_k_table(X_all, V_B)
    spacing = {g: round(float(np.hypot(*xy)), 3) for g, xy in GEOMETRIES.items()}
    cls = {}
    for d in sorted(set(spacing.values())):
        Xd = np.vstack([Xtr[f"ax_{side}_{d:g}"] for side in ("dn", "up")])
        Vd = train_rotation(Xd)
        cls[d] = (Vd, train_k_table(Xd, Vd))

    strategies = ["A", "A'", "Ak", "B", "C", "S", "D"]
    tables = {"A": K_A, "A'": K_Ap, "B": K_B}
    res = {"config": {"knots": S.KNOTS, "step_counts": STEP, "sigma_counts": SIGMA, "fs_hz": FS, "pulse_s": DUR,
                      "n_train_per_geometry": N_TRAIN, "n_test_per_geometry": N_TEST,
                      "geometries_cm": {g: list(xy) for g, xy in GEOMETRIES.items()},
                      "physics": "k U(0.0016,0.0034) cm2/s, v U(-4,32) cm/h, radial spacing error U(-10,10) %, "
                                 "lateral offset U(-0.03,0.03) cm, 0.6 cm v=0 peak U(280,420) counts, drift "
                                 "U(-40,40) mK/min + curvature U(-2,2) mK/min2, white noise 1.5 counts"},
           "sanity": chk, "k_tables": {"A": list(K_A), "A'": list(K_Ap), "B": list(K_B)},
           "spacing_class_k_tables": {f"{d:g}": list(v[1]) for d, v in cls.items()}, "geometry": {}}
    rot_dev = 0.0
    for i, g in enumerate(names):
        ts = test_set(g, N_TEST, 5000 + i)
        V_D = train_rotation(Xtr[g])
        cfg = {"A": (V_A, K_A), "A'": (V_Ap, K_Ap), "Ak": (V_A, train_k_table(Xtr[g], V_A)),
               "S": cls[spacing[g]], "B": (V_B, K_B), "C": (V_B, train_k_table(Xtr[g], V_B)), "D": (V_D, train_k_table(Xtr[g], V_D))}
        ev = {s: evaluate(ts, *cfg[s]) for s in strategies}
        p_ref = ts["rise"] @ Q @ Q.T
        rot_dev = max(rot_dev, max(float(np.max(np.abs(ev[s]["proj"] - p_ref))) for s in strategies))
        row = {"peak_counts_median": float(np.median(ts["peak"])),
               "peak_counts_p10_p90": [float(np.percentile(ts["peak"], 10)), float(np.percentile(ts["peak"], 90))],
               "k_tables": {s: list(cfg[s][1]) for s in ("Ak", "C", "S", "D")}, "strategies": {}}
        for s in strategies:
            b, r = ev[s]["bits"], ev[s]["rmse"]
            d = b - ev["A"]["bits"]
            row["strategies"][s] = {
                "coef_bits_mean": float(b.mean()), "coef_bits_p95": float(np.percentile(b, 95)),
                "delta_vs_A_bits_mean": float(d.mean()), "delta_vs_A_bits_sem": float(d.std(ddof=1) / np.sqrt(len(d))),
                "delta_vs_A_B_per_12ch_record": float(12 * d.mean() / 8),
                "rmse_vs_noisefree_mean": float(r.mean()),
                "rmse_max_abs_diff_vs_A": float(np.max(np.abs(r - ev["A"]["rmse"])))}
        res["geometry"][g] = row
        print(f"{g:15s} peak {row['peak_counts_median']:6.1f}  " + "  ".join(
            f"{s}:{row['strategies'][s]['coef_bits_mean']:6.1f}"
            f"({row['strategies'][s]['delta_vs_A_bits_sem']:.1f})" for s in strategies)
            + "  rmse " + " ".join(f"{row['strategies'][s]['rmse_vs_noisefree_mean']:.3f}" for s in strategies))
    res["sanity"]["unquantised_projection_max_abs_diff_across_V_counts"] = rot_dev
    # equal-weight mix over geometries, and the East30 pair alone
    for label, gs in (("all_geometries_mean", names), ("east30_0.6_mean", list(EAST30))):
        res[label] = {s: float(np.mean([res["geometry"][g]["strategies"][s]["coef_bits_mean"] for g in gs]))
                      for s in strategies}
        print(label, {s: round(v, 1) for s, v in res[label].items()})
    # a whole 12-channel East30-style set at one spacing: half downstream, half upstream
    res["east30_set_by_spacing"] = {}
    for d in (0.5, 0.6, 0.75, 1.0):
        gs = [f"ax_dn_{d:g}", f"ax_up_{d:g}"]
        m = {s: float(np.mean([res["geometry"][g]["strategies"][s]["coef_bits_mean"] for g in gs])) for s in strategies}
        res["east30_set_by_spacing"][f"{d:g}"] = {
            "coef_bits_per_ch": m, "delta_vs_A_B_per_12ch_record": {s: 12 * (m[s] - m["A"]) / 8 for s in strategies}}
        print(f"set {d:g} cm: " + "  ".join(f"{s}:{m[s]:.1f} ({12 * (m[s] - m['A']) / 8:+.1f} B)" for s in strategies))
    rd = max(res["geometry"][g]["strategies"][s]["rmse_max_abs_diff_vs_A"] for g in names for s in strategies)
    res["sanity"]["rmse_per_curve_max_abs_diff_vs_A_counts"] = rd
    print("unquantised projection max |diff| across rotations:", rot_dev, "; per-curve RMSE max |diff| vs A:", rd)
    C.save_json(res, OUT / "geometry.json")


if __name__ == "__main__":
    main()
