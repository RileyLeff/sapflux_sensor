#!/usr/bin/env python3
"""Experiment 4: can the compact record tell the server when to ask for raw?

The two-part encoder knows, for free, how badly each channel fits its own
representation: residual RMS of (ratiometric counts - reconstruction).  We
add it to the record as one byte per channel and check whether it (plus the
coefficients themselves) separates normal pulses from faults:

* normal corpus windows (both noise regimes) and the two real pulses
* the real Jul 11 "null" pulse (0.04 °C rise: probe not coupled)
* the real broken T1 on the Aug board (stuck near 10 counts)
* injected faults: ADC spikes, a step offset, a heater that did not fire,
  a 50 % amplitude drop on one channel (bad contact / probe pulled out)

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/exp_anomaly.py
"""

from __future__ import annotations

import copy
import warnings

import numpy as np

import common as C
import encoders as E

warnings.filterwarnings("ignore")
OUT = C.HERE / "results"


def resid_stats(w: C.Window) -> dict:
    e = E.tp2(w, k=12, q=2.0, coder="table")
    y, _ = E.ratiometric(w)
    out = {}
    for c in w.live:
        r = y[:, c] - e.recon[:, c]
        out[c] = {
            "rms": float(np.sqrt(np.mean(r**2))),
            "max": float(np.max(np.abs(r))),
            "c0": int(e.detail["coefs"][c][0]),  # KLT amplitude-like coefficient
            "mu": float(np.mean(w.raw[:, c])),
        }
    return out


def inject(w: C.Window, kind: str, rng) -> C.Window:
    w = copy.deepcopy(w)
    ch = int(rng.integers(1, 13))
    post = np.flatnonzero(w.t > 0)
    if kind == "spikes":
        idx = rng.choice(len(w.t), 3, replace=False)
        w.raw[idx, ch] += rng.choice([-1, 1], 3) * 300
    elif kind == "step":
        i0 = post[int(rng.integers(100, len(post) - 100))]
        w.raw[i0:, ch] += 40
    elif kind == "no_heat":
        clean = w.truth["clean_raw"]
        for c in w.live:  # subtract the pulse signal: rise part of the clean twin
            base = clean[:, c].copy()
            pre = w.t < 0
            p = np.polyfit(w.t[pre], base[pre], 2)
            w.raw[:, c] -= np.round(base - np.polyval(p, w.t))
    elif kind == "half_amp":
        clean = w.truth["clean_raw"]
        base = clean[:, ch]
        pre = w.t < 0
        p = np.polyfit(w.t[pre], base[pre], 2)
        w.raw[:, ch] -= np.round(0.5 * (base - np.polyval(p, w.t)))
    w.name += f"_{kind}_ch{ch}"
    w.truth = dict(w.truth, fault_channel=ch, fault=kind)
    return w


def main() -> None:
    OUT.mkdir(exist_ok=True)
    rng = np.random.default_rng(3)
    corpus = C.load_corpus()
    real = C.real_pulses()
    normal = [resid_stats(w) for w in corpus]
    rms = np.array([v["rms"] for s in normal for v in s.values()])
    mx = np.array([v["max"] for s in normal for v in s.values()])
    c0 = np.array([v["c0"] for s in normal for v in s.values()])
    thr_rms = float(np.percentile(rms, 99.5))
    thr_max = float(np.percentile(mx, 99.5))
    out = {"normal": {"rms_p50": float(np.median(rms)), "rms_p99_5": thr_rms, "max_p99_5": thr_max,
                      "c0_min": int(c0.min()), "c0_p01": float(np.percentile(c0, 1))}}
    print(out["normal"])
    for w in real:
        st = resid_stats(w)
        out[w.name] = st
        print(w.name, {c: (round(v["rms"], 2), round(v["max"], 1), v["c0"]) for c, v in st.items()})
    # Aug board T1 (broken) — not in w.live; look at it directly
    aug = real[0]
    t1 = aug.raw[:, 1]
    out["aug_T1_broken"] = {"mean_counts": float(np.mean(t1)), "sd_counts": float(np.std(t1))}
    faults = {}
    for kind in ("spikes", "step", "no_heat", "half_amp"):
        det_rms = det_max = det_c0 = 0
        n = 0
        for w in corpus[::2]:
            fw = inject(w, kind, rng)
            st = resid_stats(fw)
            if kind == "no_heat":
                hit = all(abs(v["c0"]) < 0.2 * np.median(np.abs(c0)) for v in st.values())
                det_c0 += hit
            else:
                ch = fw.truth["fault_channel"]
                det_rms += st[ch]["rms"] > thr_rms
                det_max += st[ch]["max"] > thr_max
                ref = resid_stats(w)[ch]["c0"]
                det_c0 += abs(st[ch]["c0"] - ref) > 0.25 * abs(ref)
            n += 1
        faults[kind] = {"n": n, "detected_by_rms": det_rms / n, "detected_by_max": det_max / n,
                        "detected_by_c0": det_c0 / n}
        print(kind, faults[kind])
    out["faults"] = faults
    C.save_json(out, OUT / "anomaly.json")


if __name__ == "__main__":
    main()
