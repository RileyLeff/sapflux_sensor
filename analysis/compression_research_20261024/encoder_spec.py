#!/usr/bin/env python3
"""Reference implementation of the recommended per-pulse encoder ("SFK1").

Bit-exact bitstream writer + reader, a decoder that only needs the integers
in the payload, a fixed-point emulation of the firmware arithmetic, and a
round-trip test.  Run it to (re)generate results/encoder_constants.json
(the firmware constant tables) and results/encoder_spec.json (sizes,
round-trip and fixed-point checks, science fidelity via the harness).

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/encoder_spec.py

Algorithm (per window: 60 s baseline + 181 s after heater onset, 25 Hz):

1. Ratiometric normalisation on the board, in 1/8-count units (Q3):
       y8[n,c] = round(8 * raw[n,c] * E_ref / exc13[n])
   exc13 = centred 13-sample (0.52 s) boxcar of the excitation channel
   (shrinks at the window edges), E_ref = round(mean excitation over the
   first 5 s bin).  The excitation channel itself is not transmitted.
2. Baseline: the pre-onset part is cut into NB = 5 s bins ending at t = 0
   (12 bins for 60 s).  b[i] = round(mean(y8) over bin i)  (Q3 counts).
   Decoder line: unweighted least squares through (bin centre time, b/8).
3. Rise: r[n] = y8[n]/8 - line(t[n]) for t >= 0.  Project on the 16 cubic
   B-splines in u = ln(t + 2 s) with 12 uniform interior knots over
   [ln 2, ln 183]:  g = B^T r  (4 non-zero basis values per sample).
   c = M g  with M = V^T R^-T  (R from QR of B on the 25 Hz grid, V the
   physics-trained KLT rotation; both firmware constants, 16x16).
   ci = round(c / STEP), STEP = 3.0 counts (2 x the 1.5-count noise).
   Because B^T line = a*H1 + b*Ht with H1 = B^T 1 and Ht = B^T t constant,
   the board can accumulate B^T y8 while sampling and never buffer the
   window.
4. Bitstream (MSB first), per live channel in mask order:
       19 bits  b[0] (unsigned Q3)
        4 bits  kb ; NB-1 x Rice_kb(zigzag(b[i]-b[i-1]))
        2 bits  o  ; 16 x Rice_{max(0, K[j]+o-1)}(zigzag(ci[j]))
   Rice escape: 24 one-bits then the 20-bit zigzag value.  Pad to a byte.
   Header (6 B, before the bitstream): u8 version 0x51, u16 live mask
   (bit c-1 = thermistor c), u16 E_ref, u8 NB.  Framing (run id, onset
   time, CRC) is the field protocol's and is not counted here.

Decoder: pre-onset = line + linear interpolation of the bin residuals
(b/8 - line) between bin centres; post = line + B R^-1 V (ci * STEP).
"""

from __future__ import annotations

import functools
import json
import warnings

import numpy as np

import common as C
import encoders as E
import entropy as K

warnings.filterwarnings("ignore")
OUT = C.HERE / "results"

VERSION_BASE = 0x50  # version byte = 0x50 | knots/4 (0x53: 12 knots, 0x54: 16)
FS = 25
N_POST = int(C.POST_S * FS) + 1  # 4526 samples, t = 0 .. 181 s
KNOTS = 12  # default; 16 is the higher-fidelity option
T0 = 2.0
STEP = 3.0
BIN_S = 5.0
B0_BITS = 19


def nominal_post_t() -> np.ndarray:
    return np.arange(N_POST) / FS


@functools.lru_cache(maxsize=8)
def basis(t_key: tuple, knots: int = KNOTS) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """(B, R, V) on a post-onset time grid (t >= 0)."""
    t = np.array(t_key)
    B = E.post_spline_basis(t, knots, T0)
    # (the onset sample t = 0 has an all-zero row, as in encoders.tp2)
    _, R = np.linalg.qr(B)
    # same column signs as the nominal-grid QR the KLT was trained on
    # (encoders.spline_q), so V means the same thing on any grid
    _, R_nom = np.linalg.qr(E.post_spline_basis(nominal_post_t(), knots, T0))
    R = R * (np.sign(np.diag(R)) * np.sign(np.diag(R_nom)))[:, None]
    V = E.klt_rotation(tuple(np.round(nominal_post_t(), 4)), knots, T0, 1.0)
    return B, R, V


@functools.lru_cache(maxsize=1)
def k_table(knots: int = KNOTS) -> tuple:
    return E.rice_k_table(tuple(np.round(nominal_post_t(), 4)), knots, T0, 1.0, STEP / E.SIGMA_REF)


def constants(knots: int = KNOTS) -> dict:
    t = nominal_post_t()
    B, R, V = basis(tuple(np.round(t, 4)), knots)
    M = V.T @ np.linalg.inv(R).T
    D = np.linalg.inv(R) @ V  # decoder: post = B D (ci * STEP)
    return {
        "version": VERSION_BASE | knots // 4, "fs_hz": FS, "n_post": N_POST, "knots_interior": knots, "t0_s": T0,
        "u_range": [float(np.log(T0)), float(np.log(t[-1] + T0) + 1e-9)], "step_counts": STEP,
        "bin_s": BIN_S, "rice_k_table": list(k_table(knots)), "rice_offsets": [-1, 0, 1, 2],
        "M_enc_16x16": M.tolist(), "D_dec_16x16": D.tolist(),
        "H1": B.sum(0).tolist(), "Ht": (B * t[:, None]).sum(0).tolist(),
        "notes": "c = M (G/8 - a*H1 - b*Ht), G_j = sum_n B_j(t_n) y8[n]; line = a + b t; decoder post = B D ci*STEP",
    }


# --------------------------------------------------------------------------


def ratiometric_q3(w: C.Window) -> tuple[np.ndarray, int]:
    exc = w.raw[:, C.EXC].astype(np.int64)
    n = len(exc)
    cs = np.concatenate(([0], np.cumsum(exc)))
    i = np.arange(n)
    lo, hi = np.maximum(i - 6, 0), np.minimum(i + 7, n)
    s, cnt = cs[hi] - cs[lo], hi - lo
    pre = w.t < 0
    first = pre & (w.t < w.t[0] + BIN_S - 1e-9)
    e_ref = int(np.round(np.mean(exc[first])))
    y8 = np.zeros((n, C.N_CH), np.int64)
    for c in w.live:
        num = 8 * w.raw[:, c].astype(np.int64) * e_ref * cnt
        y8[:, c] = (2 * num + s) // (2 * s)  # round half up, exact integers
    return y8, e_ref


def bins(t: np.ndarray) -> tuple[np.ndarray, list[np.ndarray]]:
    """5 s bins ending at t = 0; a leading partial bin is kept when it holds
    at least 1 s of samples (only legacy captures; the field window is an
    exact 60 s = 12 bins).  The line fit weights bins by sample count."""
    t_pre0 = t[t < 0][0]
    nb = int(np.ceil(-t_pre0 / BIN_S - 0.2 - 1e-9))
    idx = []
    for i in range(nb):
        lo, hi = -BIN_S * (nb - i), -BIN_S * (nb - i - 1)
        idx.append(np.flatnonzero((t >= lo - 1e-9) & (t < hi - 1e-9)))
    centers = np.array([t[j].mean() for j in idx])
    return centers, idx


def line_fit(tc: np.ndarray, b: np.ndarray, n: np.ndarray) -> tuple[float, float]:
    bs, a = np.polyfit(tc, b / 8.0, 1, w=np.sqrt(n))
    return float(a), float(bs)


def encode(w: C.Window, fixed_point: bool | str = False, knots: int = KNOTS) -> tuple[bytes, dict]:
    if fixed_point == "firmware":  # the Rust crate's integer arithmetic, see fw_encode
        return fw_encode(*fw_frames(w), knots=knots)
    t = w.t
    post = t >= 0
    tp = t[post]
    y8, e_ref = ratiometric_q3(w)
    B, R, V = basis(tuple(np.round(tp, 4)), knots)
    M = V.T @ np.linalg.inv(R).T
    if fixed_point:  # firmware arithmetic: Q12 basis, i64 accumulators, f32 matrix
        Bq = np.round(B * 4096).astype(np.int64)
        H1 = Bq.sum(0) / 4096.0
        Ht = (Bq * tp[:, None]).sum(0) / 4096.0
        M = M.astype(np.float32)
    else:
        H1, Ht = B.sum(0), (B * tp[:, None]).sum(0)
    tc, bidx = bins(t)
    nb = len(bidx)
    mask = sum(1 << (c - 1) for c in w.live)
    bw = K.BitWriter()
    for v, nbits in ((VERSION_BASE | knots // 4, 8), (mask, 16), (e_ref, 16), (nb, 8)):
        bw.put(v, nbits)
    kt = k_table(knots)
    ncoef = knots + 4
    out = {"coefs": {}, "base": {}}
    for c in w.live:
        s = np.array([y8[j, c].sum() for j in bidx])
        n = np.array([len(j) for j in bidx])
        b = (2 * s + n) // (2 * n)
        a, bs = line_fit(tc, b, n)
        if fixed_point:
            G = (Bq * y8[post, c][:, None]).sum(0) / 4096.0
            g = (G / 8.0 - a * H1 - bs * Ht).astype(np.float32)
            ci = np.round((M @ g).astype(np.float64) / STEP).astype(np.int64)
        else:
            r = y8[post, c] / 8.0 - (a + bs * tp)
            ci = np.round(M @ (B.T @ r) / STEP).astype(np.int64)
        d = np.diff(b)
        kb = min(range(16), key=lambda k: K.rice_len(K.zigzag(d), k))
        offs = {o: sum(K.rice_len(K.zigzag(ci[j:j + 1]), max(0, kt[j] + o)) for j in range(ncoef)) for o in (-1, 0, 1, 2)}
        o = min(offs, key=offs.get)
        bw.put(int(b[0]), B0_BITS)
        bw.put(kb, 4)
        for v in d:
            bw.rice(int(v), kb)
        bw.put(o + 1, 2)
        for j in range(ncoef):
            bw.rice(int(ci[j]), max(0, kt[j] + o))
        out["coefs"][c] = ci
        out["base"][c] = b
    return bw.bytes(), out


def decode(data: bytes, t: np.ndarray) -> tuple[np.ndarray, dict]:
    br = K.BitReader(data)
    version, mask, e_ref, nb = br.get(8), br.get(16), br.get(16), br.get(8)
    assert version >> 4 == VERSION_BASE >> 4
    knots = 4 * (version & 15)
    ncoef = knots + 4
    live = [c for c in range(1, 13) if mask >> (c - 1) & 1]
    post = t >= 0
    tp = t[post]
    B, R, V = basis(tuple(np.round(tp, 4)), knots)
    D = np.linalg.inv(R) @ V
    tc, bidx = bins(t)
    assert len(bidx) == nb
    kt = k_table(knots)
    recon = np.zeros((len(t), C.N_CH))
    recon[:, C.EXC] = e_ref
    got = {"coefs": {}, "base": {}}
    for c in live:
        b = [br.get(B0_BITS)]
        kb = br.get(4)
        for _ in range(nb - 1):
            b.append(b[-1] + br.rice(kb))
        b = np.array(b, np.int64)
        o = br.get(2) - 1
        ci = np.array([br.rice(max(0, kt[j] + o)) for j in range(ncoef)], np.int64)
        a, bs = line_fit(tc, b, np.array([len(j) for j in bidx]))
        pre = ~post
        resid = b / 8.0 - (a + bs * tc)
        recon[pre, c] = a + bs * t[pre] + np.interp(t[pre], tc, resid)
        recon[post, c] = a + bs * tp + B @ (D @ (ci * STEP))
        got["coefs"][c], got["base"][c] = ci, b
    return recon, got


def spec_encoder(w: C.Window, fixed_point: bool = False, knots: int = KNOTS) -> E.Encoded:
    data, _ = encode(w, fixed_point, knots)
    recon, _ = decode(data, w.t)
    # full-board size: header + 12 x mean live-channel bits (real windows have 5 live)
    body_bits = len(data) * 8 - 48
    per_ch = body_bits / len(w.live)
    nbytes = 6 + np.ceil(12 * per_ch / 8)
    return E.Encoded(f"sfk1_k{knots}" + ("_fixed" if fixed_point else ""), float(nbytes), recon,
                     {"payload_B_actual": len(data), "bits_per_channel": per_ch})


# --------------------------------------------------------------------------
# Firmware mode: encode(w, fixed_point="firmware") mirrors, operation for
# operation, the integer arithmetic of the Rust `sfk1` crate
# (software/sfk1/src/encoder.rs).  Same wire format; its bytes are the
# crate's golden vectors (software/sfk1/tools/generate.py).  Differences from
# the float reference above, all invisible to the decoder:
#
# * The window is the nominal 25 Hz grid: n_bins * 125 frames before onset,
#   N_POST from onset.  Frames are placed by round(25 t); empty slots are
#   *missing* frames (real captures run at ~24 Hz, so they have ~4.5 %).
#   A missing frame is left out of the excitation boxcar and of its baseline
#   bin; after onset its thermistor counts are interpolated from the frames
#   around it (the board looks 6 frames ahead anyway, for exc13), or held
#   when none is near, so the projection still sees every sample.
# * The ratio is kept unrounded as p = raw / exc13 in Q24; E_ref is applied
#   only at the end (bins) or at onset (line), so nothing waits on E_ref.
# * The baseline line is known at onset, so r = y - line is formed per
#   sample and G = B^T r accumulates directly (no H1/Ht).
# * u = ln(t + 2 s) comes from an integer log2 (degree-8 polynomial, Q30);
#   the B-spline pieces are exact integers / 12, evaluated by Horner in Q24
#   and stored as B * 12 * 2^16.
# * M is i32, scaled by 2^36 / 576; all accumulators are i64 with bounds
#   that hold for any i16 input (see the Rust sources for the proofs).
# * Results are clamped to the field widths: b to [0, 2^19), ci to
#   [-2^19, 2^19).

FW_BIN = 125  # frames per 5 s baseline bin
FW_MAX_BINS = 12
FW_HALF = 6  # exc13 = frames k-6 .. k+6
FW_P_MAX = 1 << 30  # |p| clamp (ratio < 64)
FW_RECIP_SHIFT = 40  # recip = (cnt << 40) // sum ; p = raw * recip >> 16
I32_MIN, I32_MAX = -(1 << 31), (1 << 31) - 1
FW_CI_MIN, FW_CI_MAX = -(1 << 19), (1 << 19) - 1
FW_B_MAX = (1 << B0_BITS) - 1
# log2(1 + f), f in [0, 1) as Q30; degree-8 fit at Chebyshev nodes, max
# error 5.7e-8 on the encoder's domain (v = n + 50, n = 1 .. 4525), Horner
# products < 2^60.  Lowest power first.
FW_LOG2_POLY = (61, 1549072137, -774270685, 513438044, -370901926, 255595634, -142965425, 53078675, -9304724)
# s = K ln((n+50)/50) / (ln 183 + 1e-9 - ln 2) = log2((n+50)/50) * KF;  KF in Q28
FW_KF_Q28 = {12: 535577300, 16: 700370315}
# Cubic B-spline pieces on the clamped uniform knot vector, times 12, in the
# local coordinate x in [0, 1] of an interval (lowest power first).  Row m
# is basis function i+m on interval i.  Five interval types: first, second,
# interior, second-last, last.
FW_BASIS_C12 = (
    ((12, -36, 36, -12), (0, 36, -54, 21), (0, 0, 18, -11), (0, 0, 0, 2)),
    ((3, -9, 9, -3), (7, 3, -15, 7), (2, 6, 6, -6), (0, 0, 0, 2)),
    ((2, -6, 6, -2), (8, 0, -12, 6), (2, 6, 6, -6), (0, 0, 0, 2)),
    ((2, -6, 6, -2), (8, 0, -12, 6), (2, 6, 6, -7), (0, 0, 0, 3)),
    ((2, -6, 6, -2), (7, -3, -15, 11), (3, 9, 9, -21), (0, 0, 0, 12)),
)
FW_M_SHIFT = 36  # c = sum(Mq * gx) >> 36,  Mq = round(M * 2^36 / 576)


def _fw_round_div(x: int, d: int) -> int:
    """round(x / d), halves up, for d > 0 (floor division, as Rust div_euclid)."""
    return (2 * x + d) // (2 * d)


def fw_log2_q30(v: int) -> int:
    e = v.bit_length() - 1
    f = (v << (30 - e)) - (1 << 30)
    acc = FW_LOG2_POLY[-1]
    for c in reversed(FW_LOG2_POLY[:-1]):
        acc = ((acc * f) >> 30) + c
    return (e << 30) + acc


@functools.lru_cache(maxsize=2)
def fw_basis(knots: int = KNOTS) -> tuple[np.ndarray, np.ndarray]:
    """(first index i, (N_POST, 4) values) of the integer basis: value m at
    post sample n is B_{i+m}(t_n) * 12 * 2^16 (n = 0 has no basis)."""
    nint = knots + 1
    l50 = fw_log2_q30(50)
    first = np.zeros(N_POST, np.int64)
    vals = np.zeros((N_POST, 4), np.int64)
    for n in range(1, N_POST):
        s = ((fw_log2_q30(n + 50) - l50) * FW_KF_Q28[knots] + (1 << 33)) >> 34  # Q24 knot intervals
        i, x = s >> 24, s & 0xFFFFFF
        if i >= nint:  # the last sample sits on the end knot
            i, x = nint - 1, s - ((nint - 1) << 24)
        kind = 0 if i == 0 else 1 if i == 1 else 4 if i == nint - 1 else 3 if i == nint - 2 else 2
        first[n] = i
        for m in range(4):
            c = FW_BASIS_C12[kind][m]
            h = c[3] << 24
            for cc in (c[2], c[1], c[0]):
                h = ((h * x) >> 24) + (cc << 24)
            vals[n, m] = max(0, (h + 128) >> 8)
    return first, vals


@functools.lru_cache(maxsize=2)
def fw_matrix(knots: int = KNOTS) -> tuple[tuple[int, ...], ...]:
    """M_enc from the committed constants as exact i32: round(M * 2^36 / 576)."""
    from fractions import Fraction

    M = json.loads((OUT / "encoder_constants.json").read_text())[str(knots)]["M_enc_16x16"]
    return tuple(tuple(int(round(Fraction(v) * (1 << FW_M_SHIFT) / 576)) for v in row) for row in M)


def fw_time(nb: int) -> np.ndarray:
    """Nominal time grid of a firmware record with nb baseline bins."""
    return (np.arange(nb * FW_BIN + N_POST) - nb * FW_BIN) / FS


def fw_frames(w: C.Window) -> tuple[np.ndarray, np.ndarray, list, int]:
    """Place a window's frames on the nominal 25 Hz grid the board samples on.

    Returns (raw (N, 13) int64, present (N,) bool, live, nb).  A slot no frame
    rounds to is missing; when two frames round to one slot the first wins."""
    slot = np.round(w.t * FS).astype(np.int64)
    nb = min(FW_MAX_BINS, int(np.ceil(-slot.min() / FW_BIN)))
    npre = nb * FW_BIN
    n = npre + N_POST
    raw = np.zeros((n, C.N_CH), np.int64)
    present = np.zeros(n, bool)
    vals = np.nan_to_num(w.raw, nan=0.0)
    for k, row in zip(slot + npre, vals):
        if 0 <= k < n and not present[k]:
            raw[k] = np.round(row).astype(np.int64)
            present[k] = True
    assert raw.min() >= -32768 and raw.max() <= 32767
    return raw, present, list(w.live), nb


def fw_fill(raw: np.ndarray, p: np.ndarray, present: np.ndarray, recip: np.ndarray, npre: int) -> list:
    """Post-onset p of one channel with missing frames filled.

    A missing frame k with a present frame before it and one within the next
    FW_HALF frames gets raw counts interpolated linearly between the two (in
    1/16 counts) and its own exc13 ratio; otherwise p is held from frame k-1."""
    n = len(present)
    out = []
    last_raw, last_k, last_p = 0, -1, 0
    for k in range(n):
        if present[k]:
            last_raw, last_k, last_p = int(raw[k]), k, int(p[k])
        elif k >= npre:
            nxt = next((j for j in range(k + 1, min(k + FW_HALF, n - 1) + 1) if present[j]), None)
            if nxt is not None and last_k >= 0:
                r16 = 16 * last_raw + _fw_round_div(16 * (int(raw[nxt]) - last_raw) * (k - last_k), nxt - last_k)
                last_p = min(max((r16 * int(recip[k])) >> 20, -FW_P_MAX), FW_P_MAX)
        if k >= npre:
            out.append(last_p)
    return out


def fw_encode(raw: np.ndarray, present: np.ndarray, live: list, nb: int, knots: int = KNOTS) -> tuple[bytes, dict]:
    """The Rust encoder's arithmetic on a nominal-grid window (see fw_frames)."""
    npre = nb * FW_BIN
    n = npre + N_POST
    assert raw.shape == (n, C.N_CH) and 2 <= nb <= FW_MAX_BINS
    raw = raw.astype(np.int64)
    pres = present.astype(np.int64)
    # exc13 over present frames, clipped at the window edges
    exc = raw[:, C.EXC] * pres
    cs = np.concatenate(([0], np.cumsum(exc)))
    cn = np.concatenate(([0], np.cumsum(pres)))
    k = np.arange(n)
    lo, hi = np.maximum(k - FW_HALF, 0), np.minimum(k + FW_HALF + 1, n)
    s, cnt = cs[hi] - cs[lo], cn[hi] - cn[lo]
    s = np.maximum(s, np.maximum(cnt, 1))
    recip = (np.maximum(cnt, 1) << FW_RECIP_SHIFT) // s
    p = np.zeros((n, C.N_CH), np.int64)
    for c in live:
        p[:, c] = np.clip((raw[:, c] * recip) >> 16, -FW_P_MAX, FW_P_MAX)
    # baseline bins (E_ref = rounded mean excitation of the first non-empty bin)
    e_ref = None
    b = {c: [None] * nb for c in live}
    for i in range(nb):
        sel = np.flatnonzero(present[i * FW_BIN:(i + 1) * FW_BIN]) + i * FW_BIN
        m = len(sel)
        if m == 0:
            continue
        if e_ref is None:
            e_ref = int(min(max((2 * int(raw[sel, C.EXC].sum()) + m) // (2 * m), 0), 32767))
        for c in live:
            t = (16 * e_ref * int(p[sel, c].sum()) + (m << 24)) >> 25
            t = min(max(t, I32_MIN), I32_MAX)
            b[c][i] = min(max(t // m, 0), FW_B_MAX)
    if e_ref is None:
        raise ValueError("no present frame before onset")
    ok = [i for i in range(nb) if b[live[0]][i] is not None] if live else []
    for c in live:
        for i in range(nb):
            if b[c][i] is None:
                prev = [j for j in ok if j < i]
                b[c][i] = b[c][prev[-1] if prev else ok[0]]
    # line through (bin centre, b/8), closed form, then in p units per sample
    wi = [2 * i - (nb - 1) for i in range(nb)]
    W = nb * (nb * nb - 1) // 3
    eff = max(e_ref, 1)
    first, bv = fw_basis(knots)
    ncoef = knots + 4
    Mq = fw_matrix(knots)
    out = {"coefs": {}, "base": {}, "e_ref": e_ref, "nb": nb, "missing": int(n - present.sum())}
    for c in live:
        P = sum(b[c])
        S1 = sum(x * y for x, y in zip(wi, b[c]))
        a_q16 = _fw_round_div((P * 125 * W + S1 * (125 * nb + 1) * nb) << 16, 1000 * nb * W)
        bn_q32 = _fw_round_div(S1 << 32, 500 * W)
        ap_q16 = _fw_round_div(a_q16 << 24, eff)
        bnp_q16 = _fw_round_div(bn_q32 << 8, eff)
        hp = fw_fill(raw[:, c], p[:, c], present, recip, npre)
        G = [0] * ncoef
        for t_n in range(1, N_POST):
            line = (ap_q16 + bnp_q16 * t_n + (1 << 15)) >> 16
            r = min(max(hp[t_n] - line, I32_MIN), I32_MAX)
            i0 = int(first[t_n])
            for m in range(4):
                G[i0 + m] += int(bv[t_n, m]) * r
        gx = [min(max(((g >> 20) * e_ref) >> 16, I32_MIN), I32_MAX) for g in G]
        ci = []
        for row in Mq:
            acc = sum(mq * x for mq, x in zip(row, gx))
            ci.append(min(max((acc + (1 << (FW_M_SHIFT - 1))) >> FW_M_SHIFT, FW_CI_MIN), FW_CI_MAX))
        out["coefs"][c] = np.array(ci, np.int64)
        out["base"][c] = np.array(b[c], np.int64)
    data = _fw_bitstream(live, e_ref, nb, knots, out)
    return data, out


def _fw_bitstream(live: list, e_ref: int, nb: int, knots: int, out: dict) -> bytes:
    bw = K.BitWriter()
    mask = sum(1 << (c - 1) for c in live)
    for v, nbits in ((VERSION_BASE | knots // 4, 8), (mask, 16), (e_ref, 16), (nb, 8)):
        bw.put(v, nbits)
    kt = fw_k_table(knots)
    for c in live:
        b, ci = out["base"][c], out["coefs"][c]
        d = np.diff(b)
        kb = min(range(16), key=lambda k: K.rice_len(K.zigzag(d), k))
        offs = {o: sum(K.rice_len(K.zigzag(ci[j:j + 1]), max(0, kt[j] + o)) for j in range(len(ci))) for o in (-1, 0, 1, 2)}
        o = min(offs, key=offs.get)
        bw.put(int(b[0]), B0_BITS)
        bw.put(kb, 4)
        for v in d:
            bw.rice(int(v), kb)
        bw.put(o + 1, 2)
        for j in range(len(ci)):
            bw.rice(int(ci[j]), max(0, kt[j] + o))
    return bw.bytes()


@functools.lru_cache(maxsize=2)
def fw_k_table(knots: int = KNOTS) -> tuple:
    return tuple(json.loads((OUT / "encoder_constants.json").read_text())[str(knots)]["rice_k_table"])


def fw_spec_encoder(w: C.Window, knots: int = KNOTS) -> E.Encoded:
    """spec_encoder for firmware mode: decode on the nominal grid, then
    resample to the window's own time stamps (identity for the corpus)."""
    data, info = encode(w, "firmware", knots)
    tn = fw_time(info["nb"])
    rn, _ = decode(data, tn)
    recon = np.column_stack([np.interp(w.t, tn, rn[:, c]) for c in range(C.N_CH)])
    per_ch = (len(data) * 8 - 48) / len(w.live)
    nbytes = 6 + np.ceil(12 * per_ch / 8)
    return E.Encoded(f"sfk1_k{knots}_firmware", float(nbytes), recon,
                     {"payload_B_actual": len(data), "bits_per_channel": per_ch, "missing_frames": info["missing"]})


def firmware_main() -> None:
    """Round trip + index agreement + science of the firmware mode, next to
    the float reference (results/encoder_spec.json, from main())."""
    import harness as H

    ws = H.windows()
    H.reference()
    ref = json.loads((OUT / "encoder_spec.json").read_text())
    res = {}
    for knots in (12, 16):
        mism, sizes, diff, missing = 0, [], [], []
        for w in ws:
            data, enc = encode(w, "firmware", knots)
            _, dec = decode(data, fw_time(enc["nb"]))
            for c in w.live:
                mism += int(np.any(enc["coefs"][c] != dec["coefs"][c]) or np.any(enc["base"][c] != dec["base"][c]))
            sizes.append(len(data))
            missing.append(enc["missing"])
            if w.kind == "synthetic":
                _, encf = encode(w, knots=knots)
                diff += [int(np.abs(enc["coefs"][c] - encf["coefs"][c]).max()) for c in w.live]
        e = H.evaluate(fw_spec_encoder, label=f"sfk1_k{knots}_firmware", knots=knots)
        f = ref[f"k{knots}"]["float"]
        print(f"k{knots} firmware round trip: {mism} channel mismatches over {len(ws)} windows; payload bytes mean "
              f"{np.mean(sizes):.1f}; missing frames per window max {max(missing)}; corpus index vs float: max |diff| "
              f"{max(diff)}, channels differing {np.mean(np.array(diff) > 0):.1%}")
        print(H.line(f))
        print(H.line(e))
        rr = {k: (f["rmse_ratio_vs_noisefree"][k], e["rmse_ratio_vs_noisefree"][k]) for k in e["rmse_ratio_vs_noisefree"]}
        print("   max |RMSE ratio firmware - float|:", f"{max(abs(a - b) for a, b in rr.values()):.4f}")
        res[f"k{knots}"] = {"roundtrip_channel_mismatches": mism, "payload_B_mean": float(np.mean(sizes)),
                            "max_missing_frames": int(max(missing)), "corpus_max_index_diff_vs_float": int(max(diff)),
                            "corpus_frac_channels_differing": float(np.mean(np.array(diff) > 0)),
                            "rmse_ratio_float_vs_firmware": rr, "firmware": e}
        assert mism == 0
    C.save_json(res, OUT / "encoder_spec_firmware.json")


def main() -> None:
    import harness as H

    OUT.mkdir(exist_ok=True)
    C.save_json({k: constants(k) for k in (12, 16)}, OUT / "encoder_constants.json")
    ws = H.windows()
    res = {}
    H.reference()
    for knots in (12, 16):
        # round trip: decoder recovers every integer the encoder wrote
        mism, sizes, fp_diff = 0, [], []
        for w in ws:
            data, enc = encode(w, knots=knots)
            _, dec = decode(data, w.t)
            for c in w.live:
                mism += int(np.any(enc["coefs"][c] != dec["coefs"][c]) or np.any(enc["base"][c] != dec["base"][c]))
            sizes.append(len(data))
            _, encf = encode(w, fixed_point=True, knots=knots)
            fp_diff += [int(np.abs(enc["coefs"][c] - encf["coefs"][c]).max()) for c in w.live]
        print(f"k{knots} round trip: {mism} channel mismatches over {len(ws)} windows; payload bytes mean "
              f"{np.mean(sizes):.1f} (min {min(sizes)}, max {max(sizes)}); fixed-point vs float index: max |diff| "
              f"{max(fp_diff)}, channels differing {np.mean(np.array(fp_diff) > 0):.1%}")
        r = {"roundtrip_channel_mismatches": mism, "payload_B_mean": float(np.mean(sizes)),
             "payload_B_min": int(min(sizes)), "payload_B_max": int(max(sizes)),
             "fixed_point_max_index_diff": int(max(fp_diff)),
             "fixed_point_frac_channels_differing": float(np.mean(np.array(fp_diff) > 0))}
        for fp in (False, True):
            e = H.evaluate(spec_encoder, label=f"sfk1_k{knots}" + ("_fixed" if fp else ""), fixed_point=fp, knots=knots)
            print(H.line(e))
            print("   real p95:", {k: round(v["p95"], 2) for k, v in e["shift_sigma_real"].items()
                                   if k in ("V", "eps_tau", "tau", "hrm60", "tmax_down", "peak_down")})
            r["fixed" if fp else "float"] = e
        res[f"k{knots}"] = r
        assert mism == 0
    C.save_json(res, OUT / "encoder_spec.json")


if __name__ == "__main__":
    import sys

    if "--firmware" in sys.argv[1:]:  # firmware mode vs float: round trip, indices, science
        firmware_main()
    else:
        main()
