#!/usr/bin/env python3
"""Experiment 2: per-pulse encoders, bytes vs science fidelity.

Every encoder runs on the 96-window semi-synthetic corpus (12 signal sets x
8 real-noise draws, 12 thermistors each) and the two usable real pulses.
Fidelity is reported three ways (see README):

* shift vs the raw-data estimate in units of that estimator's measurement
  sigma (p95 over all pairs/windows);
* RMSE vs the noise-free estimate, encoded / raw (1.00 = nothing lost,
  < 1 = the representation denoises);
* real-pulse max shift (sigma from Monte Carlo with real idle noise).

    uv run --with numpy --with scipy --with pandas --with matplotlib python \\
        analysis/compression_research_20261024/exp_encoders.py
"""

from __future__ import annotations

import sys
import warnings

import numpy as np

import common as C
import encoders as E
import harness as H

warnings.filterwarnings("ignore")
OUT = C.HERE / "results"


def logbins(w, frac, floor, base_w, step_rel):
    edges = E.bin_edges_log(w.t, frac, floor, base_w)
    return E.bins_quant(w, edges, step_rel, f"logbins_f{frac:g}_fl{floor:g}_b{base_w:g}_s{step_rel:g}", predictor="delta")


def ubins(w, width, step_rel):
    edges = E.bin_edges_uniform(w.t, width)
    return E.bins_quant(w, edges, step_rel, f"ratio_bins{int(width * 1000)}ms_s{step_rel:g}")


CONFIGS = [
    # reference points
    ("dayone_500ms_i16", E.bins_dayone, {}),
    ("dayone_500ms_rice", E.bins_dayone, {"coder": "rice"}),
    ("dayone_1000ms_rice", E.bins_dayone, {"width": 1.0, "coder": "rice"}),
    ("model_params_only", E.model_only, {}),
    # noise-aware uniform and log bins (ratiometric, quantized, delta+Rice)
    ("ratio_bins500_s1", ubins, {"width": 0.5, "step_rel": 1.0}),
    ("ratio_bins1000_s1", ubins, {"width": 1.0, "step_rel": 1.0}),
    ("logbins_4pct", logbins, {"frac": 0.04, "floor": 0.2, "base_w": 5.0, "step_rel": 1.0}),
    ("logbins_8pct", logbins, {"frac": 0.08, "floor": 0.4, "base_w": 5.0, "step_rel": 1.0}),
    ("logbins_15pct", logbins, {"frac": 0.15, "floor": 0.5, "base_w": 10.0, "step_rel": 1.0}),
    ("logbins_8pct_s2", logbins, {"frac": 0.08, "floor": 0.4, "base_w": 5.0, "step_rel": 2.0}),
    # two-part: baseline bins + orthonormal log-time spline
    *[(f"2part_spline{k}_q{q:g}", E.two_part, {"kind": "spline", "k": k, "q": q})
      for k in (6, 8, 10, 12, 16, 24) for q in (1.0, 2.0, 4.0)],
    ("2part_spline12_q2_b10", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "base_bin": 10.0}),
    ("2part_spline12_q2_b20", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "base_bin": 20.0}),
    ("2part_spline12_q2_b10_qb4", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "base_bin": 10.0, "qb": 4.0}),
    ("2part_spline12_q2_t0_1", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "t0": 1.0}),
    ("2part_spline12_q2_t0_5", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "t0": 5.0}),
    ("2part_spline12_q2_noratio", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "ratio": False}),
    ("2part_spline12_q2_rice", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "coder": "rice_stream"}),
    ("2part_spline12_q2_ac", E.two_part, {"kind": "spline", "k": 12, "q": 2.0, "coder": "ac"}),
    # learned (physics PCA) bases
    *[(f"2part_pca{k}_q2", E.two_part, {"kind": "pca", "k": k, "q": 2.0}) for k in (4, 6, 8, 10, 14)],
    ("2part_hybrid4_pca6_q2", E.two_part, {"kind": "hybrid4", "k": 6, "q": 2.0}),
    # piecewise-constant log bins inside the two-part frame
    ("2part_logbins8_q2", E.two_part, {"kind": "logbins", "k": 8, "q": 2.0}),
    # flash tier
    ("lossless_mean8_rice", E.lossless, {"predictor": "mean8", "coder": "rice32"}),
    ("lossless_delta_rice", E.lossless, {"predictor": "delta", "coder": "rice32"}),
    ("lossless_mean8_agolomb", E.lossless, {"predictor": "mean8", "coder": "agolomb"}),
    ("lossless_mean8_entropy", E.lossless, {"predictor": "mean8", "coder": "entropy"}),
    ("near_lossless_d1", E.near_lossless, {"delta": 1}),
    ("near_lossless_d2", E.near_lossless, {"delta": 2}),
    ("near_lossless_d4", E.near_lossless, {"delta": 4}),
]


def main() -> None:
    OUT.mkdir(exist_ok=True)
    only = set(sys.argv[1:])
    path = OUT / "encoders.json"
    import json

    results = json.loads(path.read_text()) if path.exists() and only else {}
    H.reference()
    for label, fn, kw in CONFIGS:
        if only and label not in only:
            continue
        r = H.evaluate(fn, label=label, **kw)
        results[label] = r
        print(H.line(r), flush=True)
        C.save_json(results, path)


if __name__ == "__main__":
    main()
