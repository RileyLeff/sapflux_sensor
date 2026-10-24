"""Candidate encoders.  Each takes a Window and returns an Encoded result:
total bytes for a full 12-thermistor board (scaled from the live channels
when a real window has fewer) and a reconstruction of the raw counts.

Thermistor channels are first made *ratiometric on the board*:
y = raw * E_ref / exc_0.5s, with exc_0.5s a 0.5 s boxcar of the excitation
channel and E_ref its window mean (sent once, 2 B).  The server converts y
with constant excitation E_ref.  This removes the excitation channel from
the payload and keeps the low-frequency ratiometric correction that a
constant excitation would lose (exp_noise.py: 2x sigma on V without it).
"""

from __future__ import annotations

import functools
from dataclasses import dataclass

import numpy as np
from scipy.interpolate import BSpline
from scipy.ndimage import uniform_filter1d

import entropy as K  # noqa: N812
import common as C

SIGMA_REF = 1.5  # design noise, counts at 25 Hz (quiet regime hp sigma 1.51)
HEADER_BYTES = 32  # run id, onset time, flags, E_ref, scheme id, crc


@dataclass
class Encoded:
    name: str
    nbytes: float  # full-board payload bytes (12 thermistors + excitation)
    recon: np.ndarray  # (N, 13) reconstructed raw counts
    detail: dict


def ratiometric(w: C.Window) -> tuple[np.ndarray, float]:
    exc = w.raw[:, C.EXC]
    sm = uniform_filter1d(exc, 13, mode="nearest")
    e_ref = float(np.round(np.mean(exc)))
    y = w.raw.copy()
    for c in w.live:
        y[:, c] = w.raw[:, c] * e_ref / sm
    y[:, C.EXC] = e_ref
    return y, e_ref


def scale_bytes(per_channel_bytes: list[float], exc_bytes: float = 2.0, header: float = HEADER_BYTES) -> float:
    return header + exc_bytes + 12 * float(np.mean(per_channel_bytes))


# --------------------------------------------------------------------------
# Lossless / near-lossless 25 Hz (flash tier)


def mean8_residuals(x: np.ndarray) -> np.ndarray:
    x = x.astype(np.int64)
    cs = np.concatenate(([0], np.cumsum(x)))
    i = np.arange(len(x))
    lo = np.maximum(i - 8, 0)
    n = i - lo
    pred = np.where(n > 0, (cs[i] - cs[lo]) // np.maximum(n, 1), 0)
    return x - pred


def lossless(w: C.Window, predictor: str = "mean8", coder: str = "rice32") -> Encoded:
    per = []
    for c in w.live + [C.EXC]:
        x = w.raw[:, c].astype(np.int64)
        if predictor == "mean8":
            r = mean8_residuals(x)
        elif predictor == "delta":
            r = np.diff(x, prepend=x[0])
        elif predictor == "lpc2":
            d = np.diff(x, prepend=x[0])
            r = np.diff(d, prepend=d[0])
        r = r[1:]
        bits = 16 + (K.rice_block_bits(r, 32) if coder == "rice32" else K.adaptive_golomb_bits(r) if coder == "agolomb" else K.entropy_bits(r))
        per.append(bits / 8)
    nb = scale_bytes(per[:-1], exc_bytes=per[-1], header=16)
    return Encoded(f"lossless_{predictor}_{coder}", nb, w.raw.copy(), {"bits_per_sample": 8 * np.mean(per[:-1]) / w.n})


def near_lossless(w: C.Window, delta: int, on_ratio: bool = False) -> Encoded:
    """JPEG-LS NEAR-style: mean-of-8 predictor on *reconstructed* values,
    residual quantized to bins of 2*delta+1 -> |error| <= delta counts."""
    src = w.raw
    recon = w.raw.copy()
    per = []
    for c in w.live + [C.EXC]:
        x = src[:, c].astype(np.int64)
        rec = np.zeros_like(x)
        q = np.zeros_like(x)
        rec[0] = x[0]
        hist = [int(x[0])]
        step = 2 * delta + 1
        for i in range(1, len(x)):
            h = hist[-8:]
            pred = sum(h) // len(h)
            e = int(x[i]) - pred
            qi = (abs(e) + delta) // step * (1 if e >= 0 else -1)
            q[i] = qi
            rec[i] = pred + qi * step
            hist.append(int(rec[i]))
        per.append((16 + K.rice_block_bits(q[1:], 32)) / 8)
        recon[:, c] = rec
    nb = scale_bytes(per[:-1], exc_bytes=per[-1], header=16)
    return Encoded(f"near_lossless_d{delta}", nb, recon, {"bits_per_sample": 8 * np.mean(per[:-1]) / w.n})


# --------------------------------------------------------------------------
# Bins


def bin_edges_uniform(t: np.ndarray, width: float) -> np.ndarray:
    return np.arange(t[0] - 1e-9, t[-1] + width, width)


def bin_edges_log(t: np.ndarray, frac: float, floor: float, base_w: float, tail_cap: float = 30.0) -> np.ndarray:
    """Baseline bins of base_w ending at 0; after onset bin width =
    clip(frac * t, floor, tail_cap)."""
    pre = list(np.arange(0.0, t[0] - 1e-9, -base_w)[::-1]) if t[0] < 0 else []
    edges = [float(t[0]) - 1e-9] + [e for e in pre if e > t[0]]
    x = 0.0
    if not edges or edges[-1] < 0.0:
        edges.append(0.0)
    while x < t[-1]:
        x += float(np.clip(frac * x, floor, tail_cap))
        edges.append(min(x, float(t[-1]) + 1e-6))
    return np.unique(np.array(edges))


def bin_means(t: np.ndarray, x: np.ndarray, edges: np.ndarray):
    idx = np.digitize(t, edges) - 1
    nb = len(edges) - 1
    ok = (idx >= 0) & (idx < nb) & np.isfinite(x)
    s = np.bincount(idx[ok], weights=x[ok], minlength=nb)
    n = np.bincount(idx[ok], minlength=nb)
    tc = np.bincount(idx[ok], weights=t[ok], minlength=nb)
    with np.errstate(invalid="ignore"):
        return s / n, n, tc / n


def recon_from_bins(t: np.ndarray, centers: np.ndarray, vals: np.ndarray) -> np.ndarray:
    ok = np.isfinite(vals) & np.isfinite(centers)
    return np.interp(t, centers[ok], vals[ok])


def bins_dayone(w: C.Window, width: float = 0.5, coder: str = "i16") -> Encoded:
    """docs/field-protocol.md tier: bin means of all channels incl.
    excitation, value round(16*(mean-base)) as i16."""
    edges = bin_edges_uniform(w.t, width)
    recon = w.raw.copy()
    per = []
    exc_bytes = 0.0
    for c in w.live + [C.EXC]:
        m, n, tc = bin_means(w.t, w.raw[:, c], edges)
        base = np.round(m[0])
        q = np.round(16 * (m - base)).astype(np.int64)
        if coder == "i16":
            b = 2 * len(q)
        else:
            b = (16 + K.rice_block_bits(np.diff(q), 32)) / 8
        vals = base + q / 16.0
        recon[:, c] = recon_from_bins(w.t, tc, vals)
        if c == C.EXC:
            exc_bytes = b
        else:
            per.append(b)
    nb = scale_bytes(per, exc_bytes=exc_bytes, header=90)
    return Encoded(f"bins{int(width * 1000)}ms_x16_{coder}", nb, recon, {"n_bins": len(edges) - 1})


def bins_quant(w: C.Window, edges: np.ndarray, step_rel: float, name: str, predictor: str = "delta") -> Encoded:
    """Ratiometric bin means quantized at step = step_rel * SIGMA_REF / sqrt(n)
    (noise-aware: wide bins get finer steps), predictive + Rice."""
    y, e_ref = ratiometric(w)
    recon = y.copy()
    per = []
    for c in w.live:
        m, n, tc = bin_means(w.t, y[:, c], edges)
        nn = np.maximum(n, 1)
        step = step_rel * SIGMA_REF / np.sqrt(nn)
        # quantize in a predictive loop so the step can vary per bin
        q = np.zeros(len(m), np.int64)
        rec = np.zeros(len(m))
        for i in range(len(m)):
            if predictor == "delta" or i < 2:
                pred = rec[i - 1] if i else np.round(m[0])
            else:  # linear extrapolation in bin-center time
                slope = (rec[i - 1] - rec[i - 2]) / max(tc[i - 1] - tc[i - 2], 1e-6)
                pred = rec[i - 1] + slope * (tc[i] - tc[i - 1]) * 0.5
            q[i] = int(np.round((m[i] - pred) / step[i]))
            rec[i] = pred + q[i] * step[i]
        per.append((16 + K.rice_block_bits(q[1:], 16)) / 8)
        recon[:, c] = recon_from_bins(w.t, tc, rec)
    nb = scale_bytes(per)
    return Encoded(name, nb, recon, {"n_bins": len(edges) - 1})


# --------------------------------------------------------------------------
# Basis projections


def post_spline_basis(t: np.ndarray, knots: int, t0: float = 2.0) -> np.ndarray:
    """Cubic B-splines in u = log(t + t0) on t in (0, t_end], zero before."""
    post = t > 0
    u = np.log(np.clip(t, 0, None) + t0)
    a, b = np.log(t0), u[-1] + 1e-9
    inner = np.linspace(a, b, knots + 2)[1:-1]
    kv = np.concatenate(([a] * 4, inner, [b] * 4))
    nbasis = len(kv) - 4
    B = np.zeros((len(t), nbasis))
    for j in range(nbasis):
        coef = np.zeros(nbasis)
        coef[j] = 1.0
        B[post, j] = BSpline(kv, coef, 3, extrapolate=False)(u[post])
    return np.nan_to_num(B)


def poly_basis(t: np.ndarray, degree: int) -> np.ndarray:
    s = t / 120.0
    return np.column_stack([s**d for d in range(degree + 1)])


@functools.lru_cache(maxsize=8)
def pca_vectors(n: int, t_key: tuple, dur: float, k: int, train: str = "model") -> np.ndarray:
    """Principal components of unit-peak Marshall responses (t > 0) over a
    broad parameter grid; nothing from the test corpus is used."""
    t = np.array(t_key)
    rng = np.random.default_rng(2026)
    curves = []
    for _ in range(600):
        kk = rng.uniform(0.0016, 0.0034)
        tau = 0.36 / kk
        v = rng.uniform(-4, 32)
        zeta = v * tau / (3600 * 0.6)
        eps = rng.uniform(-0.1, 0.1)
        for side in (1, -1):
            r = C.marshall_rise(t, zeta, tau, eps, side, dur)
            if r.max() > 0:
                curves.append(r / r.max())
    X = np.array(curves)
    _, _, vt = np.linalg.svd(X, full_matrices=False)
    return vt[:k].T  # (n, k)


def pre_spline_basis(t: np.ndarray, spacing: float) -> np.ndarray:
    """Cubic B-splines in t over the whole window (uniform knots every
    `spacing` s): carries the baseline and its slow wander, which the
    server's baseline fit sees in the raw data."""
    a, b = t[0] - 1e-9, t[-1] + 1e-9
    nk = max(1, int(round((b - a) / spacing)) - 1)
    inner = np.linspace(a, b, nk + 2)[1:-1]
    kv = np.concatenate(([a] * 4, inner, [b] * 4))
    nbasis = len(kv) - 4
    B = np.zeros((len(t), nbasis))
    for j in range(nbasis):
        coef = np.zeros(nbasis)
        coef[j] = 1.0
        B[:, j] = BSpline(kv, coef, 3, extrapolate=False)(t)
    return np.nan_to_num(B)


def make_basis(w: C.Window, kind: str, k: int, degree: int) -> np.ndarray:
    # degree >= 0: global polynomial baseline; degree < 0: uniform cubic
    # spline over the whole window with knots every -degree*10 s
    P = poly_basis(w.t, degree) if degree >= 0 else pre_spline_basis(w.t, -10.0 * degree)
    if kind == "spline":
        B = post_spline_basis(w.t, k)
    elif kind == "pca":
        B = pca_vectors(w.n, tuple(np.round(w.t, 4)), round(w.pulse_s, 3), k)
    elif kind.startswith("hybrid"):
        ks = int(kind[6:])  # spline interior knots
        B = np.column_stack([pca_vectors(w.n, tuple(np.round(w.t, 4)), round(w.pulse_s, 3), k), post_spline_basis(w.t, ks)])
    else:
        raise ValueError(kind)
    return np.column_stack([P, B])


def P_cols(w: C.Window, degree: int) -> int:
    return degree + 1 if degree >= 0 else pre_spline_basis(w.t[:: max(1, w.n // 50)], -10.0 * degree).shape[1]


def basis_encode(w: C.Window, kind: str, k: int, degree: int = 2, q: float = 1.0,
                 coder: str = "rice_stream", sigma_ref: float = SIGMA_REF, predictor: str = "delta") -> Encoded:
    y, e_ref = ratiometric(w)
    B = make_basis(w, kind, k, degree)
    G = B.T @ B
    Ginv = np.linalg.pinv(G)
    sd = np.sqrt(np.clip(np.diag(Ginv), 1e-30, None)) * sigma_ref  # coefficient noise sd
    step = q * sd
    recon = y.copy()
    per = []
    streams = []
    for c in w.live:
        mu = float(np.round(np.mean(y[:, c])))
        coef = Ginv @ (B.T @ (y[:, c] - mu))
        qi = np.round(coef / step).astype(np.int64)
        if predictor == "delta":
            # delta along the post-onset coefficients (adjacent B-splines are similar)
            npoly = P_cols(w, degree)
            s = np.concatenate([np.diff(qi[:npoly], prepend=0), np.diff(qi[npoly:], prepend=0)])
        else:
            s = qi
        streams.append(s)
        if coder == "rice_stream":
            npoly = P_cols(w, degree)
            bits = K.rice_stream_bits(s[:npoly]) + K.rice_stream_bits(s[npoly:])
        elif coder == "agolomb":
            bits = K.adaptive_golomb_bits(s)
        elif coder == "ac":
            bits = K.adaptive_ac_bits(s)
        else:
            bits = 16 * len(s)
        per.append((16 + bits) / 8)  # +16: channel mean
        recon[:, c] = mu + B @ (qi * step)
    nb = scale_bytes(per)
    return Encoded(f"{kind}{k}_deg{degree}_q{q}_{coder}", nb, recon,
                   {"ncoef": B.shape[1], "streams": streams})


def model_only(w: C.Window) -> Encoded:
    """Marshall parameters per pair + baseline line per channel (fitted on
    the raw data; a board can't afford this fit, it's the reference for
    'send only the answer')."""
    import pandas as pd

    import fit_4x4_pulse as fit

    temps = C.window_temps(w)
    frame = pd.DataFrame({"time_s": w.t})
    recon_t = {}
    lines = {}
    for c in w.live:
        m = (w.t >= max(-55, w.t[0] + 1)) & (w.t <= -1)
        p = np.polyfit(w.t[m], temps[c][m], 1)
        lines[c] = p
        frame[f"t{c}_rise_c"] = temps[c] - np.polyval(p, w.t)
    for pair, (d, u) in w.pairs.items():
        r, _ = fit.fit_pair(frame, pair, d, u, w.pulse_s, C.fit_args())
        for ch, amp, side in ((d, r.downstream_amplitude_c, 1), (u, r.upstream_amplitude_c, -1)):
            recon_t[ch] = np.polyval(lines[ch], w.t) + amp * C.marshall_rise(w.t, r.alpha_intercept_zeta, r.tau_s, r.epsilon, side, w.pulse_s)
    recon = w.raw.copy()
    e_ref = float(np.mean(w.raw[:, C.EXC]))
    recon[:, C.EXC] = e_ref
    for c in w.live:
        tt = recon_t.get(c, np.polyval(lines[c], w.t))
        recon[:, c] = C.temp_to_raw(tt, np.full(w.n, e_ref))
    # 6 pairs x 5 params x 2 B + 12 ch x (baseline + slope) x 2 B
    nb = HEADER_BYTES + 6 * 5 * 2 + 12 * 2 * 2
    return Encoded("model_params_only", nb, recon, {})


# --------------------------------------------------------------------------
# Two-part encoder: baseline bins + orthonormal post-onset basis


def post_basis(w: C.Window, kind: str, k: int, t0: float = 2.0) -> np.ndarray:
    """Post-onset basis columns on the post samples (t >= 0)."""
    post = w.t >= 0
    if kind == "spline":
        return post_spline_basis(w.t, k, t0)[post]
    if kind == "pca":
        return pca_vectors(w.n, tuple(np.round(w.t, 4)), round(w.pulse_s, 3), k)[post]
    if kind.startswith("hybrid"):
        ks = int(kind[6:])
        return np.column_stack([pca_vectors(w.n, tuple(np.round(w.t, 4)), round(w.pulse_s, 3), k)[post],
                                post_spline_basis(w.t, ks, t0)[post]])
    if kind == "logbins":  # piecewise-constant basis = log-spaced bin means
        edges = bin_edges_log(w.t[post], k / 100.0, 0.2, 5.0)
        idx = np.clip(np.digitize(w.t[post], edges) - 1, 0, len(edges) - 2)
        B = np.zeros((post.sum(), len(edges) - 1))
        B[np.arange(post.sum()), idx] = 1.0
        return B[:, B.sum(0) > 0]
    raise ValueError(kind)


def two_part(w: C.Window, kind: str = "spline", k: int = 16, base_bin: float = 5.0, q: float = 1.0,
             qb: float = 1.0, coder: str = "agolomb", sigma_ref: float = SIGMA_REF, t0: float = 2.0,
             quantize: bool = True, ratio: bool = True) -> Encoded:
    """Baseline: bin means over [-60, 0) (step qb*sigma/sqrt(n)).  Rise:
    data minus the baseline LSQ line extrapolated, projected on an
    orthonormalized post-onset basis Q (= B R^-1), coefficients quantized
    with a uniform step q*sigma_ref (orthonormal => every coefficient has
    noise sigma), coded with adaptive Golomb-Rice."""
    y, e_ref = ratiometric(w) if ratio else (w.raw.copy(), float(np.mean(w.raw[:, C.EXC])))
    pre = w.t < 0
    post = ~pre
    edges = np.arange(0.0, w.t[0] - 1e-9 - base_bin, -base_bin)[::-1]
    edges = np.concatenate([[w.t[0] - 1e-9], edges[edges > w.t[0]], [0.0]]) if len(edges) else np.array([w.t[0] - 1e-9, 0.0])
    edges = np.unique(edges)
    Bp = post_basis(w, kind, k, t0)
    Q, R = np.linalg.qr(Bp)
    Q = Q[:, np.abs(np.diag(R)) > 1e-8 * np.abs(np.diag(R)).max()]
    recon = y.copy()
    per = []
    streams = []
    bb, bc = [], []
    for c in w.live:
        m, n, tc = bin_means(w.t, y[:, c], edges)
        mu = float(np.round(np.nanmean(m)))
        step_b = qb * sigma_ref / np.sqrt(np.maximum(n, 1))
        qb_i = np.round((m - mu) / step_b).astype(np.int64) if quantize else (m - mu) / step_b
        mb = mu + qb_i * step_b
        # baseline line from the (quantized) bins, weighted by counts
        p = np.polyfit(tc, mb, 1, w=np.sqrt(n))
        rise = y[post, c] - np.polyval(p, w.t[post])
        coef = Q.T @ rise
        ci = np.round(coef / (q * sigma_ref)).astype(np.int64) if quantize else coef / (q * sigma_ref)
        recon[pre, c] = recon_from_bins(w.t[pre], tc, mb)
        recon[post, c] = np.polyval(p, w.t[post]) + Q @ (ci * q * sigma_ref)
        if quantize:
            sb = np.diff(qb_i, prepend=0)
            b_base = K.rice_stream_bits(sb)
            b_coef = (K.adaptive_golomb_bits(ci, init_mean=64) if coder == "agolomb"
                      else K.rice_stream_bits(ci) if coder == "rice_stream"
                      else K.adaptive_ac_bits(ci))
            bits = 16 + b_base + b_coef
            bb.append(b_base / 8)
            bc.append(b_coef / 8)
            streams.append((sb, ci))
        else:
            bits = 0
        per.append(bits / 8)
    nb = scale_bytes(per)
    return Encoded(f"2part_{kind}{k}_b{base_bin:g}_q{q:g}{'' if quantize else '_unq'}", nb, recon,
                   {"ncoef": Q.shape[1], "nbase": len(edges) - 1, "streams": streams,
                    "base_B_per_ch": float(np.mean(bb)) if bb else 0.0, "coef_B_per_ch": float(np.mean(bc)) if bc else 0.0})


# --------------------------------------------------------------------------
# Two-part v2: + learned rotation inside the spline span (KLT), baseline
# second-difference prediction, baseline reconstruction around the line.


@functools.lru_cache(maxsize=16)
def spline_q(t_key: tuple, k: int, t0: float) -> np.ndarray:
    t = np.array(t_key)
    B = post_spline_basis(t, k, t0)[t >= 0]
    Q, R = np.linalg.qr(B)
    return Q[:, np.abs(np.diag(R)) > 1e-8 * np.abs(np.diag(R)).max()]


@functools.lru_cache(maxsize=16)
def klt_rotation(t_key: tuple, k: int, t0: float, dur: float) -> np.ndarray:
    """Rotation of the orthonormal spline coefficients that compacts the
    energy of *model* curves (Marshall responses over a broad parameter
    range, plus post-onset drift curvature) into the first coefficients.
    It is trained on physics only; because it is a rotation, a mismatch
    can only cost bits, never fidelity."""
    t = np.array(t_key)
    Q = spline_q(t_key, k, t0)
    tp = t[t >= 0]
    rng = np.random.default_rng(99)
    X = []
    for _ in range(800):
        kk = rng.uniform(0.0016, 0.0034)
        tau = 0.36 / kk
        v = rng.uniform(-4, 32)
        zeta = v * tau / (3600 * 0.6)
        eps = rng.uniform(-0.1, 0.1)
        side = 1 if rng.random() < 0.5 else -1
        r = C.marshall_rise(tp, zeta, tau, eps, side, dur)
        r = 300.0 * rng.uniform(0.3, 2.0) * r / r.max()
        curv = rng.uniform(-2, 2) * 1e-3 * 277.5 * (tp / 60.0) ** 2 / 2  # counts
        X.append(Q.T @ (r + curv))
    X = np.array(X)
    _, _, vt = np.linalg.svd(X, full_matrices=False)  # uncentered: curves share a mean shape
    V = vt.T  # columns = rotated basis directions in coefficient space
    _KLT_TRAIN[(t_key, k, t0, dur)] = X @ V
    return V


_KLT_TRAIN: dict = {}


@functools.lru_cache(maxsize=16)
def rice_k_table(t_key: tuple, k: int, t0: float, dur: float, q: float, sigma_ref: float = SIGMA_REF) -> tuple:
    """Per-coefficient Rice parameter, from the physics training curves plus
    coefficient noise (sigma_ref / step in quantized units).  Firmware const."""
    klt_rotation(t_key, k, t0, dur)
    Xr = _KLT_TRAIN[(t_key, k, t0, dur)] / (q * sigma_ref)
    noise = 1.0 / q
    med = np.median(np.sqrt(Xr**2 + noise**2), axis=0)
    return tuple(int(v) for v in np.maximum(0, np.round(np.log2(med + 0.5))))


def rice_table_bits(v: np.ndarray, table: tuple, offsets=(-1, 0, 1, 2)) -> int:
    """Rice with a per-index k from the const table, shifted by a per-channel
    2-bit offset chosen by the encoder."""
    u = K.zigzag(v)
    best = None
    for o in offsets:
        b = sum(K.rice_len(u[j:j + 1], max(0, table[j] + o)) for j in range(len(u)))
        best = b if best is None else min(best, b)
    return 2 + best


def tp2(w: C.Window, k: int = 12, q: float = 2.0, base_bin: float = 5.0, qb: float = 1.0,
        rotate: bool = True, coder: str = "agolomb", t0: float = 2.0, sigma_ref: float = SIGMA_REF,
        reference: dict | None = None, name: str | None = None, base_pred: str = "slope") -> Encoded:
    """reference: {channel: previous decoded coefficient ints} for
    inter-pulse prediction (the last window the server acknowledged)."""
    y, e_ref = ratiometric(w)
    pre = w.t < 0
    post = ~pre
    t_key = tuple(np.round(w.t, 4))
    Q = spline_q(t_key, k, t0)
    V = klt_rotation(t_key, k, t0, round(w.pulse_s, 3)) if rotate else np.eye(Q.shape[1])
    QV = Q @ V
    edges = np.arange(0.0, w.t[0] - 1e-9, -base_bin)[::-1]
    edges = np.unique(np.concatenate([[w.t[0] - 1e-9], edges[edges > w.t[0]], [0.0]]))
    recon = y.copy()
    per, bb, bc, coefs = [], [], [], {}
    step = q * sigma_ref
    for c in w.live:
        m, n, tc = bin_means(w.t, y[:, c], edges)
        mu = float(np.round(np.nanmean(m)))
        sb = qb * sigma_ref / np.sqrt(np.maximum(n, 1))
        # baseline bins: quantize in a 2nd-difference predictive loop
        qi_b = np.zeros(len(m), np.int64)
        mb = np.zeros(len(m))
        # drift-aware delta: slope of the baseline bins sent first (in steps
        # per bin), each bin predicted as previous + slope
        sref = float(np.median(sb))
        slope_q = int(np.round(np.polyfit(tc, m, 1)[0] * base_bin / sref)) if base_pred == "slope" else 0
        for i in range(len(m)):
            if base_pred == "slope":
                pred = mu if i == 0 else mb[i - 1] + slope_q * sref * (tc[i] - tc[i - 1]) / base_bin
            else:
                pred = mu if i == 0 else mb[i - 1] if i == 1 else 2 * mb[i - 1] - mb[i - 2]
            qi_b[i] = int(np.round((m[i] - pred) / sb[i]))
            mb[i] = pred + qi_b[i] * sb[i]
        p = np.polyfit(tc, mb, 1, w=np.sqrt(n))
        rise = y[post, c] - np.polyval(p, w.t[post])
        cr = QV.T @ rise
        ci = np.round(cr / step).astype(np.int64)
        coefs[c] = ci
        res = ci - reference[c] if reference is not None and c in reference else ci
        line_pre = np.polyval(p, w.t[pre])
        resid = mb - np.polyval(p, tc)
        recon[pre, c] = line_pre + np.interp(w.t[pre], tc, resid)
        recon[post, c] = np.polyval(p, w.t[post]) + QV @ (ci * step)
        b_base = K.rice_stream_bits(qi_b[1:]) + 4 + 12 + (K.rice_stream_bits(np.array([slope_q])) if base_pred == "slope" else 0)
        if coder == "agolomb":
            b_coef = K.adaptive_golomb_bits(res, init_mean=16)
        elif coder == "table":
            b_coef = rice_table_bits(res, rice_k_table(t_key, k, t0, round(w.pulse_s, 3), q, sigma_ref)) if reference is None \
                else K.rice_stream_bits(res[:4]) + K.rice_stream_bits(res[4:])
        else:
            b_coef = K.rice_stream_bits(res)
        bb.append(b_base / 8)
        bc.append(b_coef / 8)
        per.append((16 + b_base + b_coef) / 8)
    nb = scale_bytes(per)
    return Encoded(name or f"tp2_k{k}_q{q:g}{'_klt' if rotate else ''}", nb, recon,
                   {"ncoef": QV.shape[1], "nbase": len(edges) - 1, "coefs": coefs,
                    "base_B_per_ch": float(np.mean(bb)), "coef_B_per_ch": float(np.mean(bc))})
