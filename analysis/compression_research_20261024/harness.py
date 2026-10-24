"""Evaluate encoders on the corpus + real pulses, in units of each
estimator's own measurement uncertainty."""

from __future__ import annotations

import functools
import zlib

import numpy as np

import common as C
import encoders as E

KEY_KINDS = ["V", "zeta", "eps_tau", "tau", "amp_down", "hrm30", "hrm60", "hrm90",
             "tmax_down", "tmax_up", "peak_down", "peak_up", "tail_down", "baseline_c"]


@functools.lru_cache(maxsize=1)
def windows() -> list[C.Window]:
    corpus = C.load_corpus()
    real = [w for w in C.real_pulses() if w.kind == "real"]
    return corpus + real


def real_clean(w: C.Window) -> np.ndarray:
    """Denoised version of a real window (poly2 + 40-knot log-time spline on
    the ratiometric counts) to which real idle noise is re-added for MC."""
    y, e_ref = E.ratiometric(w)
    B = E.make_basis(w, "spline", 40, 2)
    clean = y.copy()
    for c in w.live:
        coef, *_ = np.linalg.lstsq(B, y[:, c], rcond=None)
        clean[:, c] = B @ coef
    clean[:, C.EXC] = e_ref
    return clean


@functools.lru_cache(maxsize=1)
def real_mc_windows() -> list[C.Window]:
    out = []
    regimes = {"real_aug24_soaked4x4": "quiet", "real_jul11_bench": "july"}
    for w in windows():
        if w.kind != "real":
            continue
        clean = real_clean(w)
        pool = [nw for nw in C.noise_windows(w.n, stride_s=40.0) if nw["regime"] == regimes[w.name]]
        for j, nw in enumerate(pool[:12]):
            raw = clean.copy()
            nz = nw["raw"]
            raw[:, C.EXC] = clean[:, C.EXC] + nz[:, 0] - nz[:, 0].mean()
            for c in w.live:
                src = c if c in nw["live"] else nw["live"][c % len(nw["live"])]
                # noise enters on the raw counts, ratiometrically with the exc noise
                raw[:, c] = (clean[:, c] + nz[:, src] - nz[:, src].mean()) * raw[:, C.EXC] / clean[:, C.EXC]
            out.append(C.Window(name=f"{w.name}_mc{j}", kind="mc", regime=w.regime, t=w.t, raw=np.round(raw),
                                pairs=w.pairs, pulse_s=w.pulse_s, live=w.live, group=1000 + zlib.crc32(w.name.encode()) % 1000))
    return out


def _science(w):
    return C.science(w)


def _science_clean(w):
    """Estimates on the noise-free twin: the estimator's own answer without
    noise, so (estimate - this) isolates noise + encoding error."""
    if w.kind != "synthetic":
        return {}
    return C.science(w, w.truth["clean_raw"])


@functools.lru_cache(maxsize=1)
def reference() -> dict:
    """Raw-data estimates and per-key sigma for every window."""
    ws = windows()
    est = C.pmap(_science, ws)
    sig = C.group_sigma(ws, est)
    mc = real_mc_windows()
    mc_est = C.pmap(_science, mc)
    mc_sig = C.group_sigma(mc, mc_est)
    clean = C.pmap(_science_clean, ws)
    sigma = []
    for w, e in zip(ws, est):
        if w.kind == "synthetic":
            sigma.append({k: sig[(w.group, k)] for k in e})
        else:
            g = 1000 + zlib.crc32(w.name.encode()) % 1000
            sigma.append({k: mc_sig.get((g, k), np.nan) for k in e})
    return {"est": est, "sigma": sigma, "clean": clean}


def _run(task):
    i, fn, kwargs = task
    w = windows()[i]
    enc = fn(w, **kwargs)
    det = {k: float(v) for k, v in enc.detail.items() if isinstance(v, (int, float, np.floating, np.integer))}
    return i, enc.nbytes, C.science(w, enc.recon), enc.name, det


def evaluate(fn, label: str | None = None, **kwargs) -> dict:
    ws = windows()
    ref = reference()
    res = C.pmap(_run, [(i, fn, kwargs) for i in range(len(ws))])
    name = label or res[0][3]
    shifts = {k: [] for k in KEY_KINDS}
    shifts_real = {k: [] for k in KEY_KINDS}
    truth_err = {"enc": {k: [] for k in KEY_KINDS}, "raw": {k: [] for k in KEY_KINDS}, "sig": {k: [] for k in KEY_KINDS}}
    nbytes_syn, nbytes_real = [], []
    tmap = {"V": "v_cm_h", "zeta": "zeta", "tau": "tau_s", "eps_tau": "epsilon_tau_s"}
    details = {}
    for i, nb, sci, _, det in res:
        if ws[i].kind == "synthetic":
            for k, v in det.items():
                details.setdefault(k, []).append(v)
        w = ws[i]
        e0, s0 = ref["est"][i], ref["sigma"][i]
        (nbytes_syn if w.kind == "synthetic" else nbytes_real).append(nb)
        for key, val in sci.items():
            kind = C.kind_of(key)
            if kind not in shifts or not np.isfinite(val) or not np.isfinite(e0.get(key, np.nan)):
                continue
            s = s0.get(key, np.nan)
            if not (np.isfinite(s) and s > 0):
                continue
            (shifts if w.kind == "synthetic" else shifts_real)[kind].append((val - e0[key]) / s)
            if w.kind == "synthetic":
                tr = ref["clean"][i].get(key, np.nan)
                if np.isfinite(tr):
                    truth_err["enc"][kind].append(val - tr)
                    truth_err["raw"][kind].append(e0[key] - tr)
                    truth_err["sig"][kind].append(s)

    def summ(d):
        out = {}
        for k, v in d.items():
            a = np.abs(np.array(v))
            if len(a):
                out[k] = {"median": float(np.median(a)), "p95": float(np.percentile(a, 95)), "max": float(a.max()),
                          "bias": float(np.mean(v)), "n": int(len(a))}
        return out

    eff, excess = {}, {}
    for k in truth_err["enc"]:
        a, b, sg = (np.array(truth_err[x][k]) for x in ("enc", "raw", "sig"))
        if len(a):
            # RMSE vs the noise-free estimate, encoded / raw (1.0 = no information lost)
            eff[k] = float(np.sqrt(np.mean(a**2)) / np.sqrt(np.mean(b**2)))
            # extra error in sigma units: sqrt(max(0, MSE_enc - MSE_raw)) / sigma
            excess[k] = float(np.sqrt(max(0.0, np.mean((a / sg) ** 2) - np.mean((b / sg) ** 2))))
    return {
        "name": name, "kwargs": {k: v for k, v in kwargs.items() if not callable(v)},
        "bytes_synthetic_mean": float(np.mean(nbytes_syn)), "bytes_synthetic_p95": float(np.percentile(nbytes_syn, 95)),
        "bytes_real_mean": float(np.mean(nbytes_real)) if nbytes_real else None,
        "shift_sigma_synthetic": summ(shifts), "shift_sigma_real": summ(shifts_real),
        "rmse_ratio_vs_noisefree": eff, "excess_sigma": excess,
        "detail": {k: float(np.mean(v)) for k, v in details.items()},
    }


def worst(result: dict, kinds=("V", "eps_tau", "tau", "hrm60", "tmax_down", "peak_down", "baseline_c"), stat="p95", real=False) -> float:
    d = result["shift_sigma_real" if real else "shift_sigma_synthetic"]
    return max(d[k][stat] for k in kinds if k in d)


def line(result: dict) -> str:
    s = result["shift_sigma_synthetic"]
    r = result["shift_sigma_real"]
    g = lambda d, k, st="p95": d.get(k, {}).get(st, float("nan"))  # noqa: E731
    return (f"{result['name']:42s} {result['bytes_synthetic_mean']:8.0f} B | p95 |shift|/σ  V {g(s,'V'):.2f} εт {g(s,'eps_tau'):.2f} "
            f"τ {g(s,'tau'):.2f} hrm60 {g(s,'hrm60'):.2f} tmax {g(s,'tmax_down'):.2f} peak {g(s,'peak_down'):.2f} "
            f"tail {g(s,'tail_down'):.2f} base {g(s,'baseline_c'):.2f} | real max V {g(r,'V','max'):.2f} τ {g(r,'tau','max'):.2f} "
            f"εт {g(r,'eps_tau','max'):.2f} | RMSE ratio " + " ".join(f"{k} {result['rmse_ratio_vs_noisefree'].get(k, float('nan')):.3f}" for k in ("V", "eps_tau", "tau", "hrm60", "tmax_down", "peak_down", "tail_down")))
