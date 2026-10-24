#!/usr/bin/env python3
"""One-figure story of a heat pulse: ADC counts -> one temperature pulse ->
two ways to keep it (Implexx probe + Campbell logger, or SFK1 on our board)
-> what each leaves as bytes, next to the raw samples.

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/make_fig_comparison.py
"""

from __future__ import annotations

import warnings

import numpy as np

import common as C
import encoder_spec as S
import encoders as E
import entropy as K
import plotstyle as P

warnings.filterwarnings("ignore")

HRM_WINDOW = C.ALPHA_WINDOWS[1]  # (60, 80) s
ROW_B = 32  # bytes per row in the zoomed field layouts

# Information roles, one colour each (fixed order from the reference palette).
RAW, CURVE, BASE, DERIVED = P.SERIES[0], P.SERIES[1], P.SERIES[2], P.SERIES[3]
META = "#c9c8c3"


def sfk1_field_bytes(w: C.Window, out: dict) -> tuple[float, float]:
    """Mean per-channel baseline and coefficient bytes in this record."""
    kt = S.k_table()
    nb = nc = 0.0
    for c in w.live:
        b, ci = out["base"][c], out["coefs"][c]
        d = np.diff(b)
        kb = min(range(16), key=lambda k: K.rice_len(K.zigzag(d), k))
        nb += S.B0_BITS + 4 + K.rice_len(K.zigzag(d), kb)
        nc += 2 + min(sum(K.rice_len(K.zigzag(ci[j:j + 1]), max(0, kt[j] + o)) for j in range(len(ci)))
                      for o in (-1, 0, 1, 2))
    n = 8 * len(w.live)
    return nb / n, nc / n


def strip(ax, y0: float, fields: list, max_rows: int | None = None) -> float:
    """Lay fields (bytes, colour, label) end to end in rows of ROW_B bytes
    starting at row y0 (rows grow downward). Returns the next free row."""
    import matplotlib.patches as mp

    pos = 0.0
    for nbytes, col, label in fields:
        start, left, first = pos, nbytes, True
        while left > 1e-9:
            row, x = divmod(start, ROW_B)
            if max_rows is not None and row >= max_rows:
                break
            span = min(left, ROW_B - x)
            ax.add_patch(mp.Rectangle((x, y0 + row), span, 0.82, facecolor=col, edgecolor=P.SURFACE, linewidth=1.2))
            if first and label and span >= 1.8:
                fs = 7.5 if span >= 2.6 else 6.2
                ax.text(x + span / 2, y0 + row + 0.43, label, ha="center", va="center", fontsize=fs, color=P.INK)
            first = False
            start += span
            left -= span
        pos += nbytes
    return y0 + (np.ceil(pos / ROW_B) if max_rows is None else max_rows)


def main() -> None:
    import matplotlib.pyplot as plt
    from matplotlib.patches import FancyArrowPatch, Patch

    P.setup()
    w = C.real_pulses()[0]  # Aug 24, soaked 4x4
    ch, _ = w.pairs["p1_middle"]
    t = w.t
    data, out = S.encode(w)
    rec, _ = S.decode(data, w.t)
    y, e_ref = E.ratiometric(w)
    traw = C.raw_to_temp(y[:, ch], np.full(w.n, e_ref))
    trec = C.raw_to_temp(rec[:, ch], rec[:, C.EXC])
    t_pre = float(np.mean(traw[t < 0]))
    rise_raw, rise_rec = traw - t_pre, trec - t_pre
    tc, _ = S.bins(t)
    base_rise = C.raw_to_temp(out["base"][ch] / 8.0, np.full(len(tc), rec[0, C.EXC])) - t_pre
    base_b, coef_b = sfk1_field_bytes(w, out)
    n_frames = int(round(C.PRE_S * C.FS)) + S.N_POST
    raw_b = 13 * 2 * n_frames
    sfk1_b = 6 + 12 * (base_b + coef_b)

    fig = plt.figure(figsize=(13, 17))
    ax_exc = fig.add_axes([0.07, 0.815, 0.38, 0.085])
    ax_thm = fig.add_axes([0.57, 0.815, 0.38, 0.085])
    ax_tmp = fig.add_axes([0.30, 0.655, 0.42, 0.10])
    ax_imp = fig.add_axes([0.07, 0.46, 0.38, 0.12])
    ax_sfk = fig.add_axes([0.57, 0.505, 0.38, 0.075])
    ax_cof = fig.add_axes([0.57, 0.46, 0.38, 0.03])
    ax_sq = fig.add_axes([0.07, 0.045, 0.40, 0.32])
    ax_byt = fig.add_axes([0.55, 0.045, 0.40, 0.32])

    fig.text(0.07, 0.975, "One heat pulse, two ways to keep it", fontsize=18, weight="bold")
    fig.text(0.07, 0.958, "Real pulse from the board (soaked 4×4, Aug 24 2026), middle downstream thermistor. "
             "Byte counts are per pulse for a two-probe box, excluding timestamps.", fontsize=10, color=P.INK2)

    def step(ax, n, title, sub):
        """Title and subtitle just above an axes, placed in figure units."""
        b = ax.get_position()
        fig.text(b.x0, b.y1 + 0.022, f"{n}  {title}", fontsize=12, weight="bold", color=P.INK)
        fig.text(b.x0, b.y1 + 0.008, sub, fontsize=9, color=P.INK2)

    for ax in (ax_exc, ax_thm, ax_tmp, ax_imp, ax_sfk):
        ax.set_xlim(-62, 185)
        ax.axvline(0, color=P.MUTED, linewidth=0.8, zorder=0)

    # 1. What the ADC sees
    ax_exc.plot(t, w.raw[:, C.EXC], color=RAW, linewidth=1)
    ax_exc.set_ylabel("ADC counts")
    exc = w.raw[:, C.EXC]
    sag = float(np.mean(exc[t < 0]) - np.min(np.convolve(exc, np.ones(25) / 25, mode="valid")))
    step(ax_exc, 1, "Excitation voltage",
         f"the divider's supply as ADC counts: it sags ~{sag:.0f} counts after the pulse, so we divide it out")
    ax_thm.plot(t, w.raw[:, ch], color=RAW, linewidth=1)
    ax_thm.set_ylabel("ADC counts")
    step(ax_thm, "", "Thermistor voltage", "the same 25 Hz ADC, one of 12 channels: counts rise as the wood warms")

    # 2. One temperature pulse
    ax_tmp.plot(t, rise_raw, color=P.INK, linewidth=1)
    ax_tmp.set_ylabel("rise (°C)")
    ax_tmp.set_xlabel("seconds from heater pulse")
    step(ax_tmp, 2, "One temperature pulse", "ratiometric: counts × E_ref / excitation, then Steinhart–Hart → °C")

    # 3a. Implexx: computed down to a few numbers on the probe
    lo, hi = HRM_WINDOW
    ax_imp.axvspan(lo, hi, color=P.GRID, alpha=0.8, linewidth=0, zorder=0)
    ax_imp.plot(t, rise_raw, color=P.MUTED, alpha=0.45, linewidth=0.8, zorder=1)
    tm, pk = C.tmax(t, rise_rec)
    m = (t >= lo) & (t <= hi)
    ax_imp.plot([-60, 0], [0, 0], color=DERIVED, linewidth=3.5, solid_capstyle="round", zorder=3)
    ax_imp.plot([lo, hi], [rise_raw[m].mean()] * 2, color=DERIVED, linewidth=3.5, solid_capstyle="round", zorder=3)
    ax_imp.plot([tm, tm], [0, pk], color=DERIVED, linewidth=1.2, linestyle=(0, (2, 2)), zorder=2)
    ax_imp.scatter([tm], [pk], s=70, color=DERIVED, edgecolor=P.SURFACE, linewidth=2, zorder=4)
    ax_imp.text(-30, 0.1, "pre-pulse T", ha="center", fontsize=8.5, color=P.INK2)
    ax_imp.text(tm - 4, pk + 0.04, "tMax, ΔT", ha="right", fontsize=8.5, color=P.INK2)
    ax_imp.text(lo, 1.36, "HRM window → α, β", fontsize=8.5, color=P.INK2)
    ax_imp.text(150, 0.62, "curve itself:\nnot kept", ha="center", fontsize=8.5, color=P.MUTED, style="italic")
    ax_imp.set_ylim(-0.12, 1.5)
    ax_imp.set_ylabel("rise (°C)")
    ax_imp.set_xlabel("seconds from heater pulse")
    step(ax_imp, "3a", "Implexx probe → Campbell logger", "the probe computes ~20 numbers and sends only those")

    # 3b. SFK1: 12 baseline means + 16 curve coefficients; the curve comes back
    ax_sfk.plot(t, rise_raw, color=P.MUTED, alpha=0.45, linewidth=0.8, zorder=1)
    ax_sfk.plot(t[t >= 0], rise_rec[t >= 0], color=CURVE, linewidth=2, zorder=3)
    ax_sfk.scatter(tc, base_rise, s=26, color=BASE, edgecolor=P.SURFACE, linewidth=1.2, zorder=4)
    ax_sfk.text(-30, 0.3, "12 baseline means", ha="center", fontsize=8.5, color=P.INK2)
    ax_sfk.text(95, 1.0, "curve decoded from\n16 coefficients", fontsize=8.5, color=P.INK2)
    ax_sfk.set_ylim(-0.12, 1.5)
    ax_sfk.set_ylabel("rise (°C)")
    ax_sfk.tick_params(labelbottom=False)
    step(ax_sfk, "3b", "SFK1 on our board", "keeps the shape: baseline bins + a log-time spline projection")
    ci = np.abs(out["coefs"][ch]).astype(float)
    ax_cof.bar(np.arange(16), np.log10(ci + 1), width=0.7, color=CURVE, edgecolor=P.SURFACE)
    ax_cof.set_xlim(-0.6, 15.6)
    ax_cof.set_yticks([])
    ax_cof.set_xticks([0, 5, 10, 15])
    ax_cof.grid(False)
    ax_cof.set_xlabel("the 16 coefficients, log |value|: most are tiny, so they cost a few bits each", fontsize=8.5)

    # 4. Bytes: squares with area = bytes, all at one scale
    import matplotlib.patches as mp
    from matplotlib.patches import ConnectionPatch

    lossless_b = 35130
    step(ax_sq, 4, "What gets stored or sent  ·  area = bytes, all at one scale",
         "one pulse from a two-probe box; the small ones are zoomed on the right")
    ax_sq.set_aspect("equal")
    ax_sq.axis("off")
    side = {k: float(np.sqrt(v)) for k, v in
            (("raw", raw_b), ("lossless", lossless_b), ("sfk1", sfk1_b), ("sfa", 164), ("sc", 48))}
    R = side["raw"]
    ax_sq.add_patch(mp.Rectangle((0, 0), R, R, facecolor=RAW, edgecolor=P.SURFACE, linewidth=1.5))
    L = side["lossless"]
    ax_sq.add_patch(mp.Rectangle((0, 0), L, L, facecolor="#86b6ef", edgecolor=P.SURFACE, linewidth=1.5))
    ax_sq.text(R / 2 + L / 4, R * 0.62, f"Raw 25 Hz samples\n{raw_b:,} B", ha="center", va="center",
               fontsize=13, weight="bold", color=P.SURFACE)
    ax_sq.text(R / 2 + L / 4, R * 0.62 - 50, f"{n_frames:,} frames × 13 channels × 16 bit", ha="center",
               va="center", fontsize=9, color=P.SURFACE)
    ax_sq.text(L / 2, L / 2, f"lossless\non flash\n{lossless_b:,} B", ha="center", va="center", fontsize=9.5,
               color=P.INK)
    small = (("sfk1", CURVE), ("sfa", DERIVED), ("sc", DERIVED))
    sq_pos = {}
    xs, ys = R + 18, R - 10
    for k, col in small:
        sd = side[k]
        ys -= sd
        ax_sq.add_patch(mp.Rectangle((xs, ys), sd, sd, facecolor=col, edgecolor="none"))
        sq_pos[k] = (xs + sd, ys + sd / 2)
        ys -= 26
    ax_sq.set_xlim(-2, R + 50)
    ax_sq.set_ylim(-2, R + 2)

    # zoomed field layouts
    ax = ax_byt
    ax.set_xlim(-0.5, ROW_B + 0.5)
    ax.axis("off")
    ax.text(0, -1.9, f"Zoomed: field layout, {ROW_B} bytes per row", fontsize=10, color=P.INK2)
    yrow = 0.0
    anchors = {}

    def label(key, title, nbytes, note):
        nonlocal yrow
        anchors[key] = yrow
        ax.text(0, yrow - 0.05, title, fontsize=10.5, weight="bold", va="bottom")
        ax.text(ROW_B, yrow - 0.05, f"{nbytes:,.0f} B", fontsize=10.5, weight="bold", ha="right", va="bottom")
        ax.text(0, yrow + 0.85, note, fontsize=8.2, color=P.INK2, va="bottom")
        yrow += 1.1

    label("raw", "Raw: one 25 Hz frame", 26, f"excitation + 12 thermistors, 16 bit each; ×{n_frames:,} frames")
    yrow = strip(ax, yrow, [(2, RAW, "exc")] + [(2, RAW, f"T{k}") for k in range(1, 13)]) + 0.9

    label("sfk1", "SFK1", sfk1_b, "6 B header; per thermistor: 12 baseline means + 16 curve coefficients")
    sf = [(6, META, "hdr")]
    for k in range(1, 13):
        sf += [(base_b, BASE, f"T{k} base"), (coef_b, CURVE, f"T{k} curve")]
    yrow = strip(ax, yrow, sf) + 0.9

    label("sfa", "Implexx → Campbell: SapFlowAll", 164, "battery + panel T (FP2), 20 derived values per probe (IEEE4)")
    sa = [(2, META, "bat"), (2, META, "pnl")]
    for _ in (1, 2):
        sa += [(4, DERIVED, n) for n in ("αO", "αI", "βO", "βI", "tMO", "tMI")]
        sa += [(4, DERIVED, n) for n in ("Tp", "dT", "Ts") * 4]
        sa += [(4, DERIVED, "tMU"), (4, DERIVED, "tMU")]
    yrow = strip(ax, yrow, sa) + 0.9

    label("sc", "Implexx → Campbell: ShortCut", 48, "battery, id, SDI, then 9 derived values per probe (FP2)")
    sc = []
    for _ in (1, 2):
        sc += [(2, META, n) for n in ("bat", "id", "SDI")]
        sc += [(2, DERIVED, n) for n in ("flow", "VhO", "VhI", "αO", "αI", "βO", "βI", "tMO", "tMI")]
    yrow = strip(ax, yrow, sc)
    ax.set_ylim(yrow + 0.4, -2.3)

    for k in ("sfk1", "sfa", "sc"):
        fig.add_artist(ConnectionPatch(sq_pos[k], (-0.5, anchors[k] - 0.4), coordsA="data", coordsB="data",
                                       axesA=ax_sq, axesB=ax, color=P.MUTED, linewidth=0.9))
    fig.add_artist(ConnectionPatch((R, R), (-0.5, anchors["raw"] - 0.4), coordsA="data", coordsB="data",
                                   axesA=ax_sq, axesB=ax, color=P.MUTED, linewidth=0.9))

    handles = [Patch(color=RAW, label="raw ADC samples"), Patch(color=BASE, label="baseline means"),
               Patch(color=CURVE, label="curve coefficients"),
               Patch(color=DERIVED, label="derived numbers (curve discarded)"),
               Patch(color=META, label="header / logger fields")]
    fig.legend(handles=handles, loc="lower center", ncols=5, fontsize=9, bbox_to_anchor=(0.5, 0.012),
               handlelength=1.2, columnspacing=1.4, frameon=False)
    fig.text(0.95, 0.008, "Implexx: 4 thermistors per probe; East30 on our board: 6 per probe. "
             "3a markers are illustrative, not Implexx's exact on-probe algorithm.",
             fontsize=8, color=P.MUTED, ha="right")

    def arrow(a, b, rad=0.0):
        fig.add_artist(FancyArrowPatch(a, b, transform=fig.transFigure, arrowstyle="-|>", mutation_scale=16,
                                       color=P.MUTED, linewidth=1.4, connectionstyle=f"arc3,rad={rad}"))

    arrow((0.24, 0.79), (0.29, 0.735), rad=0.25)
    arrow((0.78, 0.79), (0.73, 0.735), rad=-0.25)
    arrow((0.36, 0.628), (0.26, 0.612), rad=0.2)
    arrow((0.66, 0.628), (0.76, 0.612), rad=-0.2)
    arrow((0.26, 0.427), (0.26, 0.41))
    arrow((0.76, 0.427), (0.76, 0.41))

    path = C.HERE / "fig_old_vs_sfk1.png"
    fig.savefig(path, dpi=170)
    print(path, f"raw {raw_b} B, SFK1 {sfk1_b:.1f} B (base {base_b:.2f} + coef {coef_b:.2f} B/thermistor)")


if __name__ == "__main__":
    main()
