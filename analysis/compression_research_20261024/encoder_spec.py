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


def encode(w: C.Window, fixed_point: bool = False, knots: int = KNOTS) -> tuple[bytes, dict]:
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
    main()
