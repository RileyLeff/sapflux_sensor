#!/usr/bin/env python3
"""Per-channel view of one heat pulse and its compressed representations.

For the Aug 24 soaked-4x4 run, plots each pair channel's detrended rise and
the downstream-minus-upstream pair difference, overlaid with:

- raw 50 Hz (identical to the lossless tier; lossless decodes bit-exact)
- 2 Hz bin averages
- 12-knot log-time cubic spline (16 coefficients per channel)
- the finite-pulse model fit from fit_4x4_pulse.py

and, underneath each, the residual (representation minus raw), smoothed with
a 1 s rolling mean against the +/-1 sigma raw-noise band. Also prints the
per-pulse byte cost of each representation, down to velocity-only.

Run with:

    uv run --with numpy --with scipy --with pandas --with matplotlib \
        --with zstandard --with lz4 python analysis/pulse_representations.py
"""

from __future__ import annotations

import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compression_study as cs  # noqa: E402
import fit_4x4_pulse as fit  # noqa: E402

OUTPUT = HERE / "outputs/compression_20261024"
KNOTS = 12
DECIMATED_HZ = 2.0
ROLLING_S = 1.0
T_MAX_S = 220.0
COLORS = {
    "raw": "#9CA3AF",
    "2 Hz": "#E69F00",
    "spline": "#2563EB",
    "model": "#DC2626",
}


def rise_columns(channels):
    return [f"t{c}_rise_c" for c in channels]


def decimate(frame: pd.DataFrame, hz: float) -> pd.DataFrame:
    edges = cs.uniform_edges(float(frame["time_s"].min()), float(frame["time_s"].max()), 1.0 / hz)
    index = np.digitize(frame["time_s"].to_numpy(float), edges) - 1
    keep = (index >= 0) & (index < len(edges) - 1)
    columns = ["time_s", *rise_columns(cs.PAIR_CHANNELS)]
    return frame.loc[keep, columns].groupby(index[keep]).mean().reset_index(drop=True)


def on_grid(reduced: pd.DataFrame, column: str, t: np.ndarray) -> np.ndarray:
    """Interpolate onto t; NaN outside the representation's own domain (the
    model is only fitted up to fit_4x4_pulse's fit_end_s)."""
    source_t = reduced["time_s"].to_numpy(float)
    values = np.interp(t, source_t, reduced[column].to_numpy(float))
    values[(t < source_t.min()) | (t > source_t.max())] = np.nan
    return values


def rolling(values: np.ndarray, hz: float) -> np.ndarray:
    window = max(1, int(round(ROLLING_S * hz)))
    return pd.Series(values).rolling(window, center=True, min_periods=1).mean().to_numpy()


def main() -> None:
    frame, run_id, period_s, pulse_s, _, noise = fit.load_capture(cs.PULSE_CAPTURE, cs.PULSE_RUN)
    noise = {int(k): float(v) for k, v in noise.items()}
    sample_hz = 1.0 / period_s
    spline_start = pulse_s + 0.2

    post = frame[frame["time_s"].between(spline_start, T_MAX_S)].reset_index(drop=True)
    t = post["time_s"].to_numpy(float)
    spline = cs.spline_reconstruction(frame, KNOTS, spline_start)
    spline = spline[spline["time_s"].le(T_MAX_S)].reset_index(drop=True)
    binned = decimate(frame, DECIMATED_HZ)

    args = cs.fit_args()
    models = {}
    for pair, (down, up) in fit.PAIRS.items():
        _, fitted = fit.fit_pair(frame, pair, down, up, pulse_s, args)
        models[down] = fitted[["time_s", "downstream_fitted_c"]].rename(columns={"downstream_fitted_c": "y"})
        models[up] = fitted[["time_s", "upstream_fitted_c"]].rename(columns={"upstream_fitted_c": "y"})

    def representations(channel: int) -> dict[str, np.ndarray]:
        column = f"t{channel}_rise_c"
        return {
            "raw": post[column].to_numpy(float),
            "2 Hz": on_grid(binned, column, t),
            "spline": spline[column].to_numpy(float),
            "model": on_grid(models[channel].rename(columns={"y": column}), column, t),
        }

    panels = []
    for pair, (down, up) in fit.PAIRS.items():
        panels.append((f"T{down} ({pair} downstream)", representations(down), noise[down]))
        panels.append((f"T{up} ({pair} upstream)", representations(up), noise[up]))
    for pair, (down, up) in fit.PAIRS.items():
        a, b = representations(down), representations(up)
        diff = {name: a[name] - b[name] for name in a}
        panels.append((f"T{down} − T{up} ({pair} pair difference)", diff, float(np.hypot(noise[down], noise[up]))))

    columns = 3
    rows = (len(panels) + columns - 1) // columns
    fig, axes = plt.subplots(
        rows * 2,
        columns,
        figsize=(17, 5.2 * rows),
        gridspec_kw={"height_ratios": [3, 1.3] * rows},
        sharex=True,
    )
    for i, (title, reps, sigma) in enumerate(panels):
        row, col = divmod(i, columns)
        top, bottom = axes[2 * row, col], axes[2 * row + 1, col]
        raw = reps["raw"]
        top.plot(t[::3], raw[::3], ".", ms=1.2, color=COLORS["raw"], alpha=0.6, label="raw 50 Hz (= lossless)")
        for name in ("2 Hz", "spline", "model"):
            top.plot(t, reps[name], color=COLORS[name], lw=1.3, label=name)
            residual = rolling(reps[name] - raw, sample_hz) * 1000
            bottom.plot(t, residual, color=COLORS[name], lw=1.0)
        bottom.axhspan(-sigma * 1000, sigma * 1000, color="#E5E7EB", zorder=0)
        bottom.axhline(0, color="#374151", lw=0.6)
        top.set_title(title, fontsize=10)
        top.grid(alpha=0.2)
        bottom.grid(alpha=0.2)
        bottom.set_ylim(-25, 25)
        if col == 0:
            top.set_ylabel("rise (°C)")
            bottom.set_ylabel("rep − raw (mK)\n1 s mean")
        if row == rows - 1:
            bottom.set_xlabel("seconds from heater onset")
    for j in range(len(panels), rows * columns):
        row, col = divmod(j, columns)
        axes[2 * row, col].axis("off")
        axes[2 * row + 1, col].axis("off")
    axes[0, 0].legend(frameon=False, fontsize=8, loc="upper right")
    fig.suptitle(
        f"Run {run_id}: channel and pair-difference traces vs compressed representations "
        f"(grey band = ±1σ raw noise)",
        fontsize=12,
    )
    fig.tight_layout()
    path = OUTPUT / "pulse_representations.png"
    fig.savefig(path, dpi=150, bbox_inches="tight")
    print(f"wrote {path}")

    # RMS of each representation against the noise-free 96-knot reference.
    reference = cs.spline_reconstruction(frame, cs.SPLINE_REFERENCE_KNOTS, spline_start)
    reference = reference[reference["time_s"].le(T_MAX_S)].reset_index(drop=True)
    print("\nRMS vs 96-knot reference (mK)")
    print(f"{'channel':>24} " + " ".join(f"{n:>8}" for n in ("raw", "2 Hz", "spline", "model")))
    for title, reps, _ in panels[:4]:
        channel = int(title[1])
        ref = reference[f"t{channel}_rise_c"].to_numpy(float)
        print(f"{title:>24} " + " ".join(f"{np.sqrt(np.nanmean((reps[n] - ref) ** 2)) * 1000:8.2f}" for n in reps))
    print(f"(model column covers only its fit window, t <= {models[2]['time_s'].max():.0f} s)")


if __name__ == "__main__":
    main()
