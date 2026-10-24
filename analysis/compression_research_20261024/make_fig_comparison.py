#!/usr/bin/env python3
"""One-figure comparison: what an Implexx probe + Campbell logger keeps from a
heat pulse vs what SFK1 keeps, at about the same bytes per thermistor.

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/make_fig_comparison.py
"""

from __future__ import annotations

import warnings

import numpy as np

import common as C
import encoder_spec as S
import encoders as E
import plotstyle as P

warnings.filterwarnings("ignore")

# Campbell SapFlowAll table (sapflux_programs): 20 IEEE4 values per probe
# plus a share of the 2 FP2 logger fields, over the probe's 4 thermistors.
IMPLEXX_B_PER_THERMISTOR = (20 * 4 + 2) / 4
HRM_WINDOW = C.ALPHA_WINDOWS[1]  # (60, 80) s


def main() -> None:
    import matplotlib.pyplot as plt

    P.setup()
    w = C.real_pulses()[0]  # Aug 24, soaked 4x4
    d, u = w.pairs["p1_middle"]
    data, _ = S.encode(w)
    rec, _ = S.decode(data, w.t)
    y, e_ref = E.ratiometric(w)
    sfk1_b = len(data) / len(w.live)

    series = []
    for i, (c, name) in enumerate(((d, "Downstream"), (u, "Upstream"))):
        traw = C.raw_to_temp(y[:, c], np.full(w.n, e_ref))
        trec = C.raw_to_temp(rec[:, c], rec[:, C.EXC])
        base = np.mean(traw[w.t < 0])
        series.append((name, P.SERIES[i], traw - base, trec - base))

    fig, (a0, a1) = plt.subplots(1, 2, figsize=(12, 5.4), sharey=True)
    fig.subplots_adjust(left=0.06, right=0.97, top=0.76, bottom=0.12, wspace=0.08)
    t = w.t
    lo, hi = HRM_WINDOW

    # Left: the curve is computed down to a few numbers on the probe.
    a0.axvspan(lo, hi, color=P.GRID, alpha=0.7, linewidth=0, zorder=0)
    for _, col, raw, rec_ in series:
        a0.plot(t, raw, color=P.MUTED, alpha=0.35, linewidth=0.8, zorder=1)
        tm, pk = C.tmax(t, rec_)
        a0.plot([tm, tm], [0, pk], color=col, linewidth=1, linestyle=(0, (2, 2)), zorder=2)
        a0.scatter([tm], [pk], s=64, color=col, edgecolor=P.SURFACE, linewidth=2, zorder=4)
        m = (t >= lo) & (t <= hi)
        a0.plot([lo, hi], [raw[m].mean()] * 2, color=col, linewidth=3, solid_capstyle="round", zorder=3)
    a0.plot([-60, 0], [0, 0], color=P.INK2, linewidth=3, solid_capstyle="round", zorder=3)
    a0.text(-30, 0.07, "pre-pulse T (each thermistor)", ha="center", fontsize=9, color=P.INK2)
    a0.text(lo, 1.5, "HRM window → α, β", ha="left", fontsize=9, color=P.INK2)
    a0.text(150, 0.68, "rest of the curve:\nnot stored", ha="center", fontsize=9, color=P.MUTED, style="italic")
    tm_d, pk_d = C.tmax(t, series[0][3])
    a0.annotate("tMax", (tm_d, pk_d), xytext=(-34, 8), textcoords="offset points", fontsize=9, color=P.INK2)

    # Right: SFK1 brings the whole curve back.
    for name, col, raw, rec_ in series:
        a1.plot(t, raw, color=col, alpha=0.22, linewidth=0.8, zorder=1)
        a1.plot(t, rec_, color=col, linewidth=2, zorder=3, label=name)
    a1.plot([], [], color=P.MUTED, alpha=0.5, linewidth=0.8, label="25 Hz raw (faint)")
    a1.legend(loc="upper right", fontsize=9, bbox_to_anchor=(0.97, 1.0))

    heads = (
        (a0, "Implexx probe → Campbell logger",
         f"~20 numbers per probe, computed on the probe  ·  ≈{IMPLEXX_B_PER_THERMISTOR:.0f} B per thermistor"),
        (a1, "SFK1",
         f"16 coefficients + 12 baseline means per thermistor  ·  ≈{sfk1_b:.0f} B per thermistor"),
    )
    for ax, title, sub in heads:
        ax.set_xlim(-62, 205)
        ax.set_ylim(-0.12, 1.6)
        ax.set_xlabel("Seconds from heater pulse")
        ax.text(0, 1.10, title, transform=ax.transAxes, fontsize=12, weight="bold", color=P.INK)
        ax.text(0, 1.04, sub, transform=ax.transAxes, fontsize=9, color=P.INK2)
        ax.axvline(0, color=P.MUTED, linewidth=0.8, zorder=0)
    a0.set_ylabel("Temperature rise (°C)")

    fig.text(0.06, 0.94, "Same bytes. One keeps a handful of numbers, the other keeps the curve.",
             fontsize=15, weight="bold", color=P.INK)
    fig.text(0.06, 0.895, "One real heat pulse (soaked 4×4, Aug 24 2026), middle thermistor pair. "
             "SFK1 lines are decoded from the compressed record.", fontsize=10, color=P.INK2)
    out = C.HERE / "fig_old_vs_sfk1.png"
    fig.savefig(out, dpi=200)
    print(out, f"SFK1 {len(data)} B for {len(w.live)} live channels = {sfk1_b:.1f} B/thermistor")


if __name__ == "__main__":
    main()
