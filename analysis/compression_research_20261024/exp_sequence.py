#!/usr/bin/env python3
"""Experiment 3: correlation across consecutive pulses.

Builds a synthetic day on one tree (2 probes x 3 depths, a pulse every
30 min, diurnal sap velocity, fixed probe geometry/misfit, battery-driven
heater-power wobble, random baseline drift, real idle noise per pulse) and
measures how many bytes the two-part encoder saves when each pulse's
quantized coefficients are coded as a difference from a reference pulse the
server already holds (the last *acknowledged* window), as a function of how
old that reference is.  Prediction is exact in the quantized domain, so
fidelity is unchanged: only bytes move.

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/exp_sequence.py
"""

from __future__ import annotations

import warnings

import numpy as np

import common as C
import encoders as E
import entropy as K

warnings.filterwarnings("ignore")
OUT = C.HERE / "results"


def diurnal_v(hour: float, v_night: float, v_peak: float) -> float:
    if 6.0 <= hour <= 20.0:
        return v_night + (v_peak - v_night) * np.sin(np.pi * (hour - 6.0) / 14.0) ** 1.5
    return v_night


def day_windows(n_pulses: int = 48, seed: int = 5, interval_h: float = 0.5) -> list[C.Window]:
    rng = np.random.default_rng(seed)
    n = int(round((C.PRE_S + C.POST_S) * C.FS)) + 1
    pool = C.noise_windows(n, stride_s=30.0)
    pool = [p for p in pool if p["regime"] in ("quiet", "interference")]
    k = 0.0025
    tau = 0.36 / k
    geo = {}
    for pname in C.FIELD_PAIRS:
        depth = pname.split("_")[1]
        geo[pname] = {
            "eps": rng.uniform(-0.05, 0.05), "peak": rng.uniform(0.7, 1.5), "gain_u": rng.uniform(0.92, 1.08),
            "mult": {"tip": 0.4, "middle": 0.7, "head": 1.0}[depth],
            "misfit": [int(rng.integers(0, 4)), float(rng.choice([-1, 1]) * rng.uniform(0.5, 1.5)),
                       int(rng.integers(0, 4)), float(rng.choice([-1, 1]) * rng.uniform(0.5, 1.5))],
        }
    wins = []
    base_temp = 15.0
    for i in range(n_pulses):
        hour = (i * interval_h) % 24
        v_head = diurnal_v(hour, 0.8, 18.0)
        power = 1.0 + 0.03 * np.sin(2 * np.pi * hour / 24) + rng.normal(0, 0.005)
        pairs = {}
        for pname, g in geo.items():
            v = v_head * g["mult"]
            zeta = v * tau / (3600 * 0.6)
            tt = np.linspace(0.05, 181, 4000)
            rd = C.marshall_rise(tt, zeta, tau, g["eps"], 1, C.PULSE_S)
            ru = C.marshall_rise(tt, zeta, tau, g["eps"], -1, C.PULSE_S)
            # same heater energy -> amplitude set at zero flow, scaled by power
            r0 = C.marshall_rise(tt, 0.0, tau, g["eps"], 1, C.PULSE_S).max()
            scale = g["peak"] / r0 * power
            pairs[pname] = {"v_cm_h": v, "zeta": zeta, "tau_s": tau, "epsilon": g["eps"],
                            "epsilon_tau_s": g["eps"] * tau, "amp_down": scale, "amp_up": scale * g["gain_u"],
                            "misfit": g["misfit"]}
        drift = 30.0 * np.sin(2 * np.pi * (hour - 9) / 24) + rng.normal(0, 5)
        base_temp += drift * interval_h * 60 / 1000.0 * 0.5
        sig = {"k_cm2_s": k, "tau_s": tau, "pairs": pairs, "drift_mK_min": float(drift),
               "drift_curv_mK_min2": float(rng.normal(0, 0.7)), "base_temp_c": float(base_temp)}
        a = pool[int(rng.integers(0, len(pool)))]
        b = pool[int(rng.integers(0, len(pool)))]
        wins.append(C.synth_window(sig, a, b, f"day_{i:02d}_{hour:04.1f}h", 2000 + i))
    return wins


def main() -> None:
    OUT.mkdir(exist_ok=True)
    wins = day_windows()
    intra = []
    coefs = []
    for w in wins:
        e = E.tp2(w, k=12, q=2.0, coder="table")
        intra.append(e.nbytes)
        coefs.append(e.detail["coefs"])
    out = {"intra_mean_B": float(np.mean(intra)), "lags": {}}
    print(f"intra (no reference): {np.mean(intra):.0f} B/pulse")
    for lag in (1, 2, 4, 12, 48):
        totals = []
        coef_bits = []
        for i in range(lag, len(wins)):
            e = E.tp2(wins[i], k=12, q=2.0, coder="table", reference=coefs[i - lag])
            totals.append(e.nbytes)
            coef_bits.append(e.detail["coef_B_per_ch"])
        if not totals:
            continue
        out["lags"][lag] = {"mean_B": float(np.mean(totals)), "coef_B_per_ch": float(np.mean(coef_bits))}
        print(f"reference {lag:2d} pulses old ({lag * 0.5:4.1f} h): {np.mean(totals):.0f} B/pulse, coef {np.mean(coef_bits):.1f} B/ch")
    # how big are the coefficient changes, per index (in quantization steps)?
    A = np.array([[coefs[i][c] for c in sorted(coefs[i])] for i in range(len(coefs))])  # (pulse, ch, coef)
    d = np.abs(np.diff(A, axis=0))
    out["median_abs_coef"] = np.median(np.abs(A), axis=(0, 1)).tolist()
    out["median_abs_delta_lag1"] = np.median(d, axis=(0, 1)).tolist()
    # best-case split Rice on lag-1 residuals vs table on intra
    res = (A[1:] - A[:-1]).reshape(-1, A.shape[2])
    out["lag1_ideal_entropy_bits_per_ch"] = float(sum(K.entropy_bits(res[:, j]) for j in range(res.shape[1])) / len(res))
    C.save_json(out, OUT / "sequence.json")
    print("median |coef| per index:", np.round(out["median_abs_coef"], 1))
    print("median |delta| lag1     :", np.round(out["median_abs_delta_lag1"], 1))


if __name__ == "__main__":
    main()
