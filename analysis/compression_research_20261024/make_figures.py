#!/usr/bin/env python3
"""Figures for the README: bytes vs fidelity, and an example reconstruction.

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/make_figures.py
"""

from __future__ import annotations

import json
import warnings

import numpy as np

import common as C
import encoder_spec as S
import encoders as E
import plotstyle as P

warnings.filterwarnings("ignore")
OUT = C.HERE / "results"
KINDS = ("V", "eps_tau", "tau", "hrm60", "peak_down", "tail_down")  # Tmax excluded: every smoother "wins" it


def worst_ratio(r: dict) -> float:
    return max(r["rmse_ratio_vs_noisefree"][k] for k in KINDS if k in r["rmse_ratio_vs_noisefree"])


def tradeoff() -> None:
    import matplotlib.pyplot as plt

    enc = json.loads((OUT / "encoders.json").read_text())
    spec = json.loads((OUT / "encoder_spec.json").read_text())
    groups = {
        "Uniform bins (day-one family)": [k for k in enc if k.startswith(("dayone", "ratio_bins"))],
        "Log-time bins": [k for k in enc if k.startswith("logbins")],
        "Two-part spline / PCA": [k for k in enc if k.startswith(("2part", "tp2"))],
        "Lossless / near-lossless 25 Hz": [k for k in enc if k.startswith(("lossless", "near"))],
        "Model parameters only": ["model_params_only"],
    }
    fig, ax = plt.subplots(figsize=(9, 5))
    for i, (g, keys) in enumerate(groups.items()):
        xs = [enc[k]["bytes_synthetic_mean"] for k in keys]
        ys = [worst_ratio(enc[k]) for k in keys]
        ax.scatter(xs, ys, s=36, color=P.SERIES[i], label=g, edgecolor=P.SURFACE, linewidth=1.5, zorder=3)
    sx = 6 + spec["k12"]["payload_B_mean"] + 26  # same 32 B framing allowance as the sweep
    sy = worst_ratio(spec["k12"]["float"])
    ax.scatter([sx], [sy], s=140, marker="*", color=P.SERIES[5], label="SFK1 k12 (recommended)", zorder=4,
               edgecolor=P.SURFACE, linewidth=1.5)
    notes = {"dayone_500ms_i16": "day-one plan", "dayone_500ms_rice": "same values, Rice",
             "logbins_4pct": "log bins 4%", "logbins_15pct": "log bins 15%", "model_params_only": "params only",
             "lossless_mean8_rice": "lossless", "near_lossless_d4": "near-lossless ±4",
             "2part_spline6_q4": "too few knots"}
    for k, lab in notes.items():
        if k in enc:
            ax.annotate(lab, (enc[k]["bytes_synthetic_mean"], worst_ratio(enc[k])), xytext=(6, 4),
                        textcoords="offset points", fontsize=8, color=P.INK2)
    ax.annotate("SFK1", (sx, sy), xytext=(6, -12), textcoords="offset points", fontsize=9, color=P.INK, weight="bold")
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.axhline(1.0, color=P.INK2, linewidth=1)
    ax.axhspan(1.0, 1.05, color=P.SERIES[2], alpha=0.08, linewidth=0)
    ax.set_xlabel("Bytes per pulse, full 12-thermistor board (incl. 32 B framing allowance)")
    ax.set_ylabel("Worst RMSE ratio vs raw (V, εт, τ, HRM60, peak, tail)")
    ax.set_title("Bytes vs science fidelity: 1.0 = nothing lost, shaded = within 5%")
    ax.set_yticks([1, 1.05, 1.2, 1.5, 2, 3], ["1.00", "1.05", "1.2", "1.5", "2", "3"])
    ax.legend(fontsize=8, loc="upper right")
    fig.tight_layout()
    fig.savefig(C.HERE / "fig_tradeoff.png", dpi=160)


def reconstruction() -> None:
    import matplotlib.pyplot as plt

    w = C.real_pulses()[0]  # Aug 24 soaked 4x4
    d, u = w.pairs["p1_middle"]
    data, _ = S.encode(w)
    rec, _ = S.decode(data, w.t)
    y, e_ref = E.ratiometric(w)
    day1 = E.bins_dayone(w)
    fig, ax = plt.subplots(2, 1, figsize=(9, 5.6), sharex=True, gridspec_kw={"height_ratios": [2.2, 1]})
    for i, (c, lab) in enumerate(((d, "downstream T2"), (u, "upstream T5"))):
        traw = C.raw_to_temp(y[:, c], np.full(w.n, e_ref))
        trec = C.raw_to_temp(rec[:, c], rec[:, C.EXC])  # decoder output carries its own E_ref
        base = np.mean(traw[w.t < 0])
        ax[0].plot(w.t, traw - base, color=P.SERIES[i], alpha=0.35, linewidth=1, label=f"{lab}: 25 Hz raw")
        ax[0].plot(w.t, trec - base, color=P.SERIES[i], linewidth=2, label=f"{lab}: SFK1 decode")
        ax[1].plot(w.t, 1e3 * (traw - trec), color=P.SERIES[i], linewidth=0.8, alpha=0.8)
    td = C.raw_to_temp(day1.recon[:, d], C.smooth_excitation(day1.recon[:, 0], C.EXC_MODE, w.t))
    ax[0].plot(w.t, td - np.mean(td[w.t < 0]), color=P.INK2, linewidth=1, linestyle=":", label="day-one 500 ms bins (T2)")
    ax[0].set_ylabel("Rise (°C)")
    nbytes = len(data) * 12 / len(w.live)
    ax[0].set_title(f"Real Aug 24 pulse (soaked 4x4): SFK1 ≈ {nbytes:.0f} B for 12 channels vs 12.6 kB day-one")
    ax[0].legend(fontsize=8, ncols=2)
    ax[1].set_ylabel("raw − decode (mK)")
    ax[1].set_xlabel("Time from heater onset (s)")
    ax[1].set_ylim(-30, 30)
    fig.tight_layout()
    fig.savefig(C.HERE / "fig_reconstruction.png", dpi=160)


def main() -> None:
    P.setup()
    tradeoff()
    reconstruction()


if __name__ == "__main__":
    main()
