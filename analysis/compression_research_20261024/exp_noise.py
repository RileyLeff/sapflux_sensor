#!/usr/bin/env python3
"""Experiment 1: what the noise looks like, and how much excitation we need.

* per noise regime: count noise, spectrum, cross-channel correlation
* excitation handling in the ratiometric conversion (per-sample vs
  smoothed vs window line) -> temperature noise and science sigma
* sensitivity of temperature to excitation error (how many bits)

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/exp_noise.py
"""

from __future__ import annotations

import numpy as np
from scipy.ndimage import uniform_filter1d
from scipy.signal import welch

import common as C
import plotstyle as P

OUT = C.HERE / "results"
EXC_MODES = ["per_sample", "ma13", "ma125", "window_line", "mean"]


def regime_stats() -> tuple[dict, dict]:
    stats, psds = {}, {}
    for session, regime, raw, live in C.idle_blocks():
        x = raw[:, live]
        t_hp = x - uniform_filter1d(x, 101, axis=0, mode="nearest")
        d_sigma = np.median(np.abs(np.diff(x, axis=0)), axis=0) / 0.6745 / np.sqrt(2)
        corr = np.corrcoef(t_hp.T)
        off = corr[~np.eye(len(live), dtype=bool)]
        # sigma of 1 s means relative to white-noise expectation
        n = 25
        m = len(x) // n
        means = x[: m * n].reshape(m, n, -1).mean(1)
        dm = np.diff(means, axis=0) / np.sqrt(2)
        ratio = np.median(np.std(dm - np.median(dm, 0), 0) / (np.std(t_hp, 0) / np.sqrt(n)))
        fr, pxx = welch(x - x.mean(0), fs=C.FS, nperseg=2048, axis=0)
        psds[regime] = (fr, np.median(pxx, axis=1))
        stats[regime] = {
            "session": session,
            "seconds": len(x) / C.FS,
            "sigma_counts_hp_25Hz": float(np.median(np.std(t_hp, 0))),
            "sigma_counts_diff_25Hz": float(np.median(d_sigma)),
            "exc_sigma_counts": float(np.std(raw[:, 0] - uniform_filter1d(raw[:, 0], 101, mode="nearest"))),
            "mean_abs_cross_corr": float(np.mean(np.abs(off))),
            "sd_of_1s_means_vs_white": float(ratio),
        }
    return stats, psds


def exc_temperature_noise() -> dict:
    out = {}
    for session, regime, raw, live in C.idle_blocks():
        t = np.arange(len(raw)) / C.FS
        res = {}
        for mode in EXC_MODES:
            # window-length segments (241 s) so 'mean'/'line' mean what they do in the field
            n = int(241 * C.FS)
            vals = []
            for s in range(0, len(raw) - n + 1, n):
                seg = raw[s:s + n]
                exc = C.smooth_excitation(seg[:, 0], mode, t[s:s + n])
                for c in live:
                    temp = C.raw_to_temp(seg[:, c], exc)
                    vals.append(np.std(temp - uniform_filter1d(temp, 251, mode="nearest")))
            res[mode] = 1e3 * float(np.median(vals))
        out[regime] = res
    return out


def _sci(args):
    w, mode = args
    return C.science(w, exc_mode=mode)


def exc_science_sigma(corpus) -> dict:
    out = {}
    base = None
    for mode in EXC_MODES:
        est = C.pmap(_sci, [(w, mode) for w in corpus])
        sig = C.group_sigma(corpus, est)
        if base is None:
            base = sig
        ratios = {}
        for kind in ["V", "eps_tau", "tau", "hrm60", "tmax_down", "peak_down", "baseline_c"]:
            # probe 1 only: in the synthetic corpus probe 2's idle noise comes
            # from a different time than the excitation channel
            r = [sig[k] / base[k] for k in sig if C.kind_of(k[1]) == kind and np.isfinite(sig[k]) and base[k] > 0
                 and (k[1].startswith("p1_") or k[1].split(".")[0] in {f"ch{i}" for i in range(1, 7)})]
            ratios[kind] = float(np.median(r))
        out[mode] = ratios
    return out


def exc_sensitivity() -> dict:
    """mK of temperature error per count of excitation error, near 20 °C."""
    exc = 24890.0
    raw = C.temp_to_raw(np.array([20.0, 21.0]), np.array([exc, exc]))
    t0 = C.raw_to_temp(raw, np.array([exc, exc]))
    t1 = C.raw_to_temp(raw, np.array([exc + 1, exc + 1]))
    return {"abs_mK_per_exc_count": float(1e3 * (t1 - t0)[0]),
            "rise_error_mK_per_exc_count_on_1C_rise": float(1e3 * ((t1 - t0)[1] - (t1 - t0)[0])),
            "counts_per_degC": float(raw[0] - raw[1])}


def main() -> None:
    OUT.mkdir(exist_ok=True)
    P.setup()
    stats, psds = regime_stats()
    temp_noise = exc_temperature_noise()
    corpus = C.load_corpus()
    sci = exc_science_sigma(corpus)
    sens = exc_sensitivity()
    C.save_json({"regimes": stats, "exc_temperature_noise_mK": temp_noise,
                 "exc_science_sigma_ratio_vs_per_sample": sci, "exc_sensitivity": sens},
                OUT / "noise.json")
    import matplotlib.pyplot as plt

    fig, ax = plt.subplots(1, 2, figsize=(11, 3.8))
    labels = {"quiet": "Aug 24 quiet (after pulse)", "interference": "Aug 24 idle, 9.8 Hz interference",
              "july": "Jul 11 idle, 9.6 Hz interference"}
    for regime, (fr, p) in psds.items():
        ax[0].semilogy(fr[1:], p[1:], label=labels[regime])
    ax[0].set_xlabel("Frequency (Hz), 25 Hz sampling")
    ax[0].set_ylabel("Thermistor PSD (counts²/Hz), median channel")
    ax[0].set_title("Two noise regimes: white-ish vs a ~9.7 Hz tone")
    ax[0].legend(fontsize=8)
    modes = EXC_MODES
    x = np.arange(len(modes))
    for i, regime in enumerate(temp_noise):
        ax[1].bar(x + (i - 1) * 0.27, [temp_noise[regime][m] for m in modes], 0.25, label=labels[regime])
    ax[1].set_xticks(x, ["per sample", "0.5 s MA", "5 s MA", "window line", "window mean"])
    ax[1].set_ylabel("Temperature noise (mK), 25 Hz")
    ax[1].set_title("Excitation used in the ratiometric conversion")
    fig.tight_layout()
    fig.savefig(C.HERE / "fig_noise.png", dpi=160)
    import json

    print(json.dumps({"regimes": stats, "temp_noise": temp_noise, "sci": sci, "sens": sens}, indent=1))


if __name__ == "__main__":
    main()
