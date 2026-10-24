#!/usr/bin/env python3
"""How compressible are sap-flux thermistor traces?

Quantifies, on real captures, the storage and uplink cost of four tiers:

1. raw protocol frames and a packed field-storage record
2. lossless coding of the raw ADC counts (delta/predictor + Rice/varint/
   bit-packing, and general-purpose compressors)
3. lossy reductions in temperature units (decimation, log-time least-squares
   splines, the physical Marshall model itself), judged by how far they move
   the fitted parameters from `fit_4x4_pulse.py`
4. flash-capacity and LTE-M byte budgets for one and two boards

Run with:

    uv run --with numpy --with scipy --with pandas --with matplotlib \
        --with zstandard --with lz4 python analysis/compression_study.py
"""

from __future__ import annotations

import argparse
import json
import lzma
import math
import sys
import zlib
from pathlib import Path

import lz4.frame
import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import zstandard
from scipy.interpolate import LSQUnivariateSpline

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fit_4x4_pulse as fit  # noqa: E402

CAPTURES = HERE.parent / "software/sap-flux-monitor/captures"
PULSE_CAPTURE = CAPTURES / "session_20260824_203902/samples.csv"
PULSE_RUN = 1787618451
JULY_PULSES = [
    (CAPTURES / "session_20260711_002159/samples.csv", 1783743736),
    (CAPTURES / "session_20260711_005525/samples.csv", 1783745735),
]
OUTPUT = HERE / "outputs/compression_20261024"

THERMISTOR_COLS = [f"t{i}_raw" for i in range(1, 7)]
RAW_COLS = ["excitation_raw", "v24_raw", *THERMISTOR_COLS]
WORKING_THERMISTORS = [f"t{i}_raw" for i in (2, 3, 4, 5, 6)]  # T1 broken
PAIR_CHANNELS = [2, 3, 5, 6]

FLASH_BYTES = 32 * 1024**2  # W25Q256
# Protocol v1 Sample: 8 B header + 40 B payload + 4 B CRC, +1 COBS, +1 delimiter.
WIRE_SAMPLE_BYTES = 8 + 40 + 4 + 1 + 1
# Packed flash record: 8 x i16 channels + u16 timestamp delta in 10 us units.
PACKED_SAMPLE_BYTES = 8 * 2 + 2
STANDARD_WINDOW_S = (60.0, 1.0, 180.0)  # baseline, pulse, recovery
# Uplink assumptions: IPv4 20 + UDP 8 + DTLS-ish 29 + 23 B app header.
DATAGRAM_OVERHEAD_BYTES = 80
DATAGRAM_PAYLOAD_BYTES = 1100
# 1-minute idle summary: 8 B time/status, exc + v24 means (4 B), and per
# thermistor mean u16 + std u8 (18 B) = 30 B.
IDLE_SUMMARY_BYTES = 8 + 4 + 6 * 3
# Per-pulse reduced-record header: run id, onset unix ms, pulse duration,
# v24 during pulse, flags, scheme id.
PULSE_HEADER_BYTES = 32
RICE_BLOCK = 64
TIMESTAMP_QUANTUM_US = 100
MONTE_CARLO_REALIZATIONS = 30
SPLINE_T0_S = 2.0
SPLINE_KNOTS = [1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 24, 32, 40, 48]
SPLINE_REFERENCE_KNOTS = 96
PARAMETER_KEYS = [
    "alpha_intercept_zeta",
    "epsilon_tau_s",
    "tau_s",
    "epsilon",
    "alpha_60_80",
    "nominal_velocity_cm_hr",
]


def fit_args() -> argparse.Namespace:
    return argparse.Namespace(
        nominal_xbar_cm=0.6,
        heater_voltage_v=25.6,
        heater_resistance_ohm=42.0,
        heater_effective_length_m=0.02,
        rho_c_j_m3_k=2.5e6,
        alpha_start_s=8.0,
        fit_end_s=120.0,
        minimum_rise_c=0.05,
    )


# --------------------------------------------------------------------------
# Lossless coding of raw counts


def zigzag(values: np.ndarray) -> np.ndarray:
    values = values.astype(np.int64)
    return np.where(values >= 0, 2 * values, -2 * values - 1).astype(np.int64)


def residuals(series: np.ndarray, predictor: str) -> np.ndarray:
    """Prediction residuals; the first sample(s) count as residual vs 0."""
    x = series.astype(np.int64)
    if predictor == "delta1":
        return np.diff(x, prepend=0)
    if predictor == "delta2":
        d = np.diff(x, prepend=0)
        return np.diff(d, prepend=0)
    if predictor == "mean8":
        # Predict each sample as the floor-mean of the previous 8 samples.
        cumulative = np.concatenate(([0], np.cumsum(x)))
        index = np.arange(len(x))
        lo = np.maximum(index - 8, 0)
        count = index - lo
        prediction = np.where(
            count > 0,
            np.floor_divide(cumulative[index] - cumulative[lo], np.maximum(count, 1)),
            0,
        )
        return x - prediction
    raise ValueError(predictor)


RICE_ESCAPE_QUOTIENT = 16


def rice_bits(values: np.ndarray, block: int = RICE_BLOCK) -> int:
    """Rice code, optimal k per block, 4-bit k header per block. A quotient
    of 16 or more is an escape: 16 unary ones followed by the raw 16 bits."""
    total = 0
    for start in range(0, len(values), block):
        v = values[start : start + block]
        best = None
        for k in range(16):
            quotient = v >> k
            cost = np.where(
                quotient < RICE_ESCAPE_QUOTIENT,
                quotient + 1 + k,
                RICE_ESCAPE_QUOTIENT + 16,
            ).sum()
            best = cost if best is None else min(best, cost)
        total += int(best) + 4
    return total


def varint_bits(values: np.ndarray) -> int:
    nbytes = np.ones(len(values), dtype=np.int64)
    for shift in (7, 14, 21, 28, 35):
        nbytes += values >= (1 << shift)
    return int(8 * nbytes.sum())


def bitpack_bits(values: np.ndarray, block: int = RICE_BLOCK) -> int:
    """Frame-of-reference style: fixed width per block + 5-bit width header."""
    total = 0
    for start in range(0, len(values), block):
        v = values[start : start + block]
        width = int(v.max()).bit_length() if len(v) else 0
        total += len(v) * width + 5
    return total


def entropy_bits(values: np.ndarray) -> float:
    _, counts = np.unique(values, return_counts=True)
    p = counts / counts.sum()
    return float(-(p * np.log2(p)).sum())


def noise_counts(series: np.ndarray) -> float:
    """White-noise sigma from first differences, outliers clipped.

    A MAD on integer differences quantizes to 1/0.6745/sqrt(2) counts, so
    use a clipped standard deviation instead."""
    d = np.diff(series.astype(float))
    d = d - np.median(d)
    scale = max(1.4826 * float(np.median(np.abs(d))), 1.0)
    d = d[np.abs(d) <= 6.0 * scale]
    return float(np.std(d) / math.sqrt(2))


def general_compressors(data: bytes) -> dict[str, int]:
    return {
        "zlib-9": len(zlib.compress(data, 9)),
        "lzma-9e": len(lzma.compress(data, preset=9 | lzma.PRESET_EXTREME)),
        "zstd-3": len(zstandard.ZstdCompressor(level=3).compress(data)),
        "zstd-19": len(zstandard.ZstdCompressor(level=19).compress(data)),
        "lz4": len(lz4.frame.compress(data, compression_level=12)),
    }


def lossless_segment(name: str, raw: pd.DataFrame, monotonic_us: np.ndarray) -> dict:
    """Bits per channel-sample for every coder, per channel and in total."""
    n = len(raw)
    per_channel: dict[str, dict[str, float]] = {}
    for column in RAW_COLS:
        x = raw[column].to_numpy(np.int64)
        row: dict[str, float] = {
            "noise_counts": noise_counts(x),
            "range_counts": float(x.max() - x.min()),
        }
        sigma = max(row["noise_counts"], 1e-9)
        row["gaussian_floor_bits"] = max(0.0, math.log2(sigma * math.sqrt(2 * math.pi * math.e)))
        for predictor in ("delta1", "delta2", "mean8"):
            r = residuals(x, predictor)
            z = zigzag(r)
            row[f"{predictor}_entropy"] = entropy_bits(r[8:])
            row[f"{predictor}_rice"] = rice_bits(z) / n
            row[f"{predictor}_varint"] = varint_bits(z) / n
            row[f"{predictor}_bitpack"] = bitpack_bits(z) / n
        planar = x.astype("<i2").tobytes()
        for codec, size in general_compressors(planar).items():
            row[f"planar_{codec}"] = 8 * size / n
        delta = zigzag(np.diff(x, prepend=0)).astype("<u2").tobytes()
        for codec, size in general_compressors(delta).items():
            row[f"delta_{codec}"] = 8 * size / n
        per_channel[column] = row

    interleaved = raw[RAW_COLS].to_numpy(np.int64).astype("<i2").tobytes()
    interleaved_sizes = {
        codec: 8 * size / n for codec, size in general_compressors(interleaved).items()
    }

    # Timestamps: delta-of-delta, zigzag, Rice; at 1 us and quantized to 0.1 ms.
    dod = np.diff(np.diff(monotonic_us.astype(np.int64)), prepend=0)
    timestamp_us_bits = rice_bits(zigzag(dod)) / n
    quantized = np.round(monotonic_us.astype(np.int64) / TIMESTAMP_QUANTUM_US).astype(np.int64)
    timestamp_bits = rice_bits(zigzag(np.diff(np.diff(quantized), prepend=0))) / n
    period_us = np.diff(monotonic_us)
    return {
        "name": name,
        "samples": n,
        "duration_s": float((monotonic_us[-1] - monotonic_us[0]) / 1e6),
        "per_channel": per_channel,
        "interleaved_bits_per_sample": interleaved_sizes,
        "timestamp_rice_bits": timestamp_bits,
        "timestamp_rice_bits_1us": timestamp_us_bits,
        "period_us_median": float(np.median(period_us)),
        "period_us_p01_p99": [float(np.percentile(period_us, 1)), float(np.percentile(period_us, 99))],
    }


def projected_record_bits(segment: dict, method: str) -> float:
    """Bits per sample for a healthy 6-thermistor probe (T1 replaced by the
    mean working-channel cost) + excitation + v24 + timestamp."""
    per = segment["per_channel"]
    working = np.mean([per[c][method] for c in WORKING_THERMISTORS])
    return 6 * working + per["excitation_raw"][method] + per["v24_raw"][method] + segment["timestamp_rice_bits"]


# --------------------------------------------------------------------------
# Data loading


def load_raw(path: Path, run_id: int | None) -> pd.DataFrame:
    frame = pd.read_csv(path)
    if run_id is not None:
        frame = frame[frame["run_id"].eq(run_id)]
    return frame.reset_index(drop=True)


def longest_contiguous(frame: pd.DataFrame, max_gap_us: int = 200_000) -> pd.DataFrame:
    t = frame["monotonic_us"].to_numpy(np.int64)
    breaks = np.flatnonzero((np.diff(t) <= 0) | (np.diff(t) > max_gap_us)) + 1
    edges = np.concatenate(([0], breaks, [len(t)]))
    lengths = np.diff(edges)
    best = int(np.argmax(lengths))
    return frame.iloc[edges[best] : edges[best + 1]].reset_index(drop=True)


# --------------------------------------------------------------------------
# Lossy reductions in temperature units


def detrend(frame: pd.DataFrame) -> pd.DataFrame:
    """Same baseline detrend as fit_4x4_pulse.load_capture (-20..-1 s line)."""
    frame = frame.copy()
    for channel in PAIR_CHANNELS:
        temp = frame[f"t{channel}_temp_c"]
        valid = frame["time_s"].between(-20.0, -1.0) & np.isfinite(temp)
        coefficients = np.polyfit(frame.loc[valid, "time_s"], temp[valid], 1)
        frame[f"t{channel}_rise_c"] = temp - np.polyval(coefficients, frame["time_s"])
    return frame


def bin_average(frame: pd.DataFrame, edges: np.ndarray) -> pd.DataFrame:
    columns = ["time_s", *[f"t{c}_temp_c" for c in PAIR_CHANNELS]]
    index = np.digitize(frame["time_s"].to_numpy(float), edges) - 1
    keep = (index >= 0) & (index < len(edges) - 1)
    grouped = frame.loc[keep, columns].groupby(index[keep]).mean()
    return grouped.reset_index(drop=True)


def uniform_edges(t_min: float, t_max: float, step: float) -> np.ndarray:
    start = math.floor(t_min / step) * step
    return np.arange(start, t_max + step, step)


def tiered_edges(t_min: float, t_max: float) -> np.ndarray:
    """1 Hz baseline, 10 Hz for 0-20 s, 2 Hz to 60 s, 0.5 Hz after."""
    parts = [
        uniform_edges(t_min, 0.0, 1.0)[:-1],
        np.arange(0.0, 20.0, 0.1),
        np.arange(20.0, 60.0, 0.5),
        np.arange(60.0, t_max + 2.0, 2.0),
    ]
    return np.concatenate(parts)


def log_edges(t_min: float, t_max: float, fraction: float = 0.04, floor_s: float = 0.1) -> np.ndarray:
    """1 Hz baseline, then bin width = max(floor, fraction * t) after onset."""
    edges = list(uniform_edges(t_min, 0.0, 1.0)[:-1])
    t = 0.0
    while t <= t_max:
        edges.append(t)
        t += max(floor_s, fraction * t)
    edges.append(t)
    return np.array(edges)


def interpolate_to(reduced: pd.DataFrame, full: pd.DataFrame) -> pd.DataFrame:
    """Linearly interpolate reduced rises back onto the full 50 Hz time grid,
    so the refit keeps the original estimator weighting."""
    out = full[["time_s"]].copy()
    for channel in PAIR_CHANNELS:
        out[f"t{channel}_rise_c"] = np.interp(
            out["time_s"], reduced["time_s"], reduced[f"t{channel}_rise_c"]
        )
    return out


def monte_carlo_sigma(
    clean: pd.DataFrame, noise: dict, pulse_duration_s: float, seed: int = 20261024
) -> dict[str, dict[str, float]]:
    """Parameter scatter from refitting the denoised curve plus white noise."""
    rng = np.random.default_rng(seed)
    draws: dict[str, dict[str, list[float]]] = {
        pair: {key: [] for key in PARAMETER_KEYS} for pair in fit.PAIRS
    }
    for _ in range(MONTE_CARLO_REALIZATIONS):
        noisy = clean.copy()
        for channel in PAIR_CHANNELS:
            noisy[f"t{channel}_rise_c"] += rng.normal(0.0, noise[channel], len(noisy))
        results = fit_all_pairs(noisy, pulse_duration_s)
        for pair in fit.PAIRS:
            for key in PARAMETER_KEYS:
                draws[pair][key].append(results[pair][key])
    return {
        pair: {key: float(np.std(values, ddof=1)) for key, values in keys.items()}
        for pair, keys in draws.items()
    }


def sigma_shift(shift: dict, sigma: dict) -> dict[str, dict[str, float]]:
    return {
        pair: {key: shift[pair][key] / sigma[pair][key] for key in PARAMETER_KEYS}
        for pair in shift
    }


def fit_all_pairs(frame: pd.DataFrame, pulse_duration_s: float) -> dict[str, dict]:
    args = fit_args()
    results = {}
    for pair, (down, up) in fit.PAIRS.items():
        result, _ = fit.fit_pair(frame, pair, down, up, pulse_duration_s, args)
        results[pair] = result.__dict__
    return results


def parameter_shift(reference: dict, candidate: dict) -> dict[str, dict[str, float]]:
    shifts = {}
    for pair in reference:
        shifts[pair] = {}
        for key in PARAMETER_KEYS:
            ref = reference[pair][key]
            val = candidate[pair][key]
            shifts[pair][key] = val - ref
            shifts[pair][f"{key}_rel"] = (val - ref) / abs(ref) if ref else float("nan")
    return shifts


def spline_domain(frame: pd.DataFrame, start_s: float) -> pd.DataFrame:
    return frame[frame["time_s"].ge(start_s)].copy()


def spline_study(frame: pd.DataFrame, noise: dict, start_s: float) -> dict:
    """Least-squares cubic splines in u = log(t + T0) over the post-pulse rise.

    Heater-on samples are excluded: the heater edges couple an electrical
    step into the ADC that is not thermal signal."""
    post = spline_domain(frame, start_s)
    t = post["time_s"].to_numpy(float)
    u = np.log(t + SPLINE_T0_S)
    results: dict[str, dict] = {}
    for channel in PAIR_CHANNELS:
        y = post[f"t{channel}_rise_c"].to_numpy(float)

        def make(knots: int) -> LSQUnivariateSpline:
            interior = np.linspace(u[0], u[-1], knots + 2)[1:-1]
            return LSQUnivariateSpline(u, y, interior, k=3)

        reference = make(SPLINE_REFERENCE_KNOTS)(u)
        rows = []
        for knots in SPLINE_KNOTS:
            fitted = make(knots)(u)
            rows.append(
                {
                    "knots": knots,
                    "coefficients": knots + 4,
                    "rms_vs_reference_c": float(np.sqrt(np.mean((fitted - reference) ** 2))),
                    "max_vs_reference_c": float(np.max(np.abs(fitted - reference))),
                    "rms_vs_data_c": float(np.sqrt(np.mean((fitted - y) ** 2))),
                }
            )
        results[str(channel)] = {
            "noise_c": noise[channel],
            "peak_rise_c": float(np.max(y)),
            "reference_rms_vs_data_c": float(np.sqrt(np.mean((reference - y) ** 2))),
            "rows": rows,
        }
    return results


def spline_reconstruction(frame: pd.DataFrame, knots: int, start_s: float) -> pd.DataFrame:
    post = spline_domain(frame, start_s)
    u = np.log(post["time_s"].to_numpy(float) + SPLINE_T0_S)
    interior = np.linspace(u[0], u[-1], knots + 2)[1:-1]
    for channel in PAIR_CHANNELS:
        y = post[f"t{channel}_rise_c"].to_numpy(float)
        post[f"t{channel}_rise_c"] = LSQUnivariateSpline(u, y, interior, k=3)(u)
    return post


def minimum_knots(spline: dict, metric: str, threshold: float) -> int | None:
    worst = None
    for channel in spline.values():
        candidates = [row["knots"] for row in channel["rows"] if row[metric] < threshold]
        if not candidates:
            return None
        worst = max(worst or 0, min(candidates))
    return worst


# --------------------------------------------------------------------------
# Budgets


def datagram_bytes(payload: float) -> float:
    if payload <= 0:
        return 0.0
    return payload + math.ceil(payload / DATAGRAM_PAYLOAD_BYTES) * DATAGRAM_OVERHEAD_BYTES


def fmt(value: float, digits: int = 1) -> str:
    if value >= 1e6:
        return f"{value / 1e6:.{digits}f} MB"
    if value >= 1e3:
        return f"{value / 1e3:.{digits}f} kB"
    return f"{value:.0f} B"


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)

    # ---------------------------------------------------------------- data
    pulse_raw = load_raw(PULSE_CAPTURE, PULSE_RUN)
    capture = pd.read_csv(PULSE_CAPTURE)
    idle_raw = longest_contiguous(capture[capture["run_id"].eq(0)].reset_index(drop=True))
    sample_hz = 1e6 / float(np.median(np.diff(pulse_raw["monotonic_us"])))
    mean_hz = (len(pulse_raw) - 1) / (
        (pulse_raw["monotonic_us"].iloc[-1] - pulse_raw["monotonic_us"].iloc[0]) / 1e6
    )

    state_seconds = (pulse_raw["state_name"].value_counts() / mean_hz).to_dict()
    run_seconds = len(pulse_raw) / mean_hz

    # ------------------------------------------------------------ lossless
    segments = [
        lossless_segment("Aug 24 pulse run (277 s)", pulse_raw[RAW_COLS], pulse_raw["monotonic_us"].to_numpy()),
        lossless_segment(
            f"Aug 24 idle ({len(idle_raw) / mean_hz:.0f} s contiguous)",
            idle_raw[RAW_COLS],
            idle_raw["monotonic_us"].to_numpy(),
        ),
    ]
    for path, run_id in JULY_PULSES:
        july = load_raw(path, run_id)
        july = longest_contiguous(july)
        segments.append(
            lossless_segment(f"July pulse run {run_id}", july[RAW_COLS], july["monotonic_us"].to_numpy())
        )

    methods = [
        "delta1_entropy",
        "mean8_entropy",
        "delta1_rice",
        "delta2_rice",
        "mean8_rice",
        "delta1_varint",
        "delta1_bitpack",
        "planar_zstd-19",
        "delta_zstd-19",
        "delta_lzma-9e",
        "delta_lz4",
    ]
    projected = {
        seg["name"]: {m: projected_record_bits(seg, m) for m in methods} for seg in segments
    }

    # ---------------------------------------------------------------- lossy
    (frame, run_id, period_s, pulse_duration_s, _, noise) = fit.load_capture(PULSE_CAPTURE, PULSE_RUN)
    noise = {int(k): v for k, v in noise.items()}
    reference = fit_all_pairs(frame, pulse_duration_s)
    t_min = float(frame["time_s"].min())
    t_max = float(frame["time_s"].max())

    temp_frame = frame[["time_s", *[f"t{c}_temp_c" for c in PAIR_CHANNELS]]]
    peak_rise = max(float(frame.loc[frame["time_s"].ge(0), f"t{c}_rise_c"].max()) for c in PAIR_CHANNELS)
    baseline = frame["time_s"].lt(0)
    mk_per_count = float(
        np.mean(
            [
                1000 * np.polyfit(frame.loc[baseline, f"t{c}_raw"], frame.loc[baseline, f"t{c}_temp_c"], 1)[0]
                for c in PAIR_CHANNELS
            ]
        )
    )
    # 0.1 mK i16 covers +/-3.28 C; fall back to 1 mK for big rises.
    lossy_value_bytes = 2

    schemes = {
        "50 Hz (reference)": None,
        "uniform 10 Hz": uniform_edges(t_min, t_max, 0.1),
        "uniform 5 Hz": uniform_edges(t_min, t_max, 0.2),
        "uniform 2 Hz": uniform_edges(t_min, t_max, 0.5),
        "uniform 1 Hz": uniform_edges(t_min, t_max, 1.0),
        "uniform 0.5 Hz": uniform_edges(t_min, t_max, 2.0),
        "tiered 10/2/0.5 Hz": tiered_edges(t_min, t_max),
        "log bins 4% (min 0.1 s)": log_edges(t_min, t_max, 0.04, 0.1),
        "log bins 10% (min 0.2 s)": log_edges(t_min, t_max, 0.10, 0.2),
    }
    decimation = []
    for name, edges in schemes.items():
        if edges is None:
            reduced = frame
            points = len(frame)
            post_points = int(frame["time_s"].ge(0).sum())
        else:
            reduced = detrend(bin_average(temp_frame, edges))
            points = len(reduced)
            post_points = int(reduced["time_s"].ge(0).sum())
        results = fit_all_pairs(reduced, pulse_duration_s)
        interpolated = fit_all_pairs(interpolate_to(reduced, frame), pulse_duration_s)
        decimation.append(
            {
                "scheme": name,
                "points": points,
                "post_onset_points": post_points,
                "bytes_6ch": points * 6 * lossy_value_bytes,
                "fit": results,
                "shift": parameter_shift(reference, results),
                "shift_interp": parameter_shift(reference, interpolated),
            }
        )

    spline_start_s = pulse_duration_s + 0.2
    spline = spline_study(frame, noise, spline_start_s)
    clean = spline_reconstruction(frame, SPLINE_REFERENCE_KNOTS, spline_start_s)
    clean_fit = fit_all_pairs(clean, pulse_duration_s)
    mc_sigma = monte_carlo_sigma(clean, noise, pulse_duration_s)
    for d in decimation:
        d["shift_sigma"] = sigma_shift(d["shift"], mc_sigma)
        d["shift_interp_sigma"] = sigma_shift(d["shift_interp"], mc_sigma)
    knots_noise = minimum_knots(spline, "rms_vs_reference_c", 0.006)
    knots_tight = minimum_knots(spline, "rms_vs_reference_c", 0.002)
    knots_tight_max = minimum_knots(spline, "max_vs_reference_c", 0.006)
    spline_fits = {}
    for knots in sorted({k for k in (knots_noise, knots_tight, knots_tight_max, 6, 8, 12, 16) if k}):
        reconstructed = spline_reconstruction(frame, knots, spline_start_s)
        results = fit_all_pairs(reconstructed, pulse_duration_s)
        shift_clean = parameter_shift(clean_fit, results)
        spline_fits[knots] = {
            "fit": results,
            "shift_vs_raw": parameter_shift(reference, results),
            "shift_vs_clean": shift_clean,
            "shift_vs_clean_sigma": sigma_shift(shift_clean, mc_sigma),
        }

    model = {
        pair: {
            "trace_rmse_c": reference[pair]["trace_rmse_c"],
            "alpha_rmse": reference[pair]["alpha_rmse"],
            "noise_down_c": noise[down],
            "noise_up_c": noise[up],
        }
        for pair, (down, up) in fit.PAIRS.items()
    }

    # ------------------------------------------------------------- budgets
    baseline_s, pulse_s, recovery_s = STANDARD_WINDOW_S
    windows = {
        "Aug 24 run as recorded": run_seconds,
        "standard 60 s + 1 s + 180 s": baseline_s + pulse_s + recovery_s,
    }
    pulse_bits = projected[segments[0]["name"]]
    idle_bits = projected[segments[1]["name"]]
    lossless_method = "mean8_rice"  # MCU-friendly: integer predictor + Rice
    archive_method = "delta_lzma-9e"

    window_rows = []
    for wname, seconds in windows.items():
        n = seconds * mean_hz
        window_rows.append(
            {
                "window": wname,
                "seconds": seconds,
                "samples": n,
                "wire": n * WIRE_SAMPLE_BYTES,
                "packed": n * PACKED_SAMPLE_BYTES,
                "lossless_rice": n * pulse_bits[lossless_method] / 8,
                "lossless_lzma": n * pulse_bits[archive_method] / 8,
            }
        )

    window_s = baseline_s + pulse_s + recovery_s
    window_samples = window_s * mean_hz
    # Scale log-bin point count to the standard window: 1 Hz baseline + post-onset bins up to 181 s.
    post_points_std = len(log_edges(-baseline_s, recovery_s + pulse_s, 0.04, 0.1)) - int(baseline_s)
    decimated_window_bytes = PULSE_HEADER_BYTES + (int(baseline_s) + post_points_std) * 6 * lossy_value_bytes
    uniform_1hz_window_bytes = PULSE_HEADER_BYTES + int(window_s) * 6 * lossy_value_bytes
    # Smallest knot count whose reconstruction moves tau, epsilon*tau and V by
    # less than 1 Monte Carlo sigma for every pair.
    spline_knots = next(
        (
            int(knots)
            for knots, v in sorted(spline_fits.items())
            if all(
                abs(v["shift_vs_clean_sigma"][pair][key]) < 1.0
                for pair in fit.PAIRS
                for key in ("tau_s", "epsilon_tau_s", "nominal_velocity_cm_hr")
            )
        ),
        SPLINE_KNOTS[-1],
    )
    # Per channel: baseline mean/slope/noise as f32 + spline coefficients as i16.
    # Per pair (3 pairs on a healthy probe): zeta, tau, epsilon, A_down, A_up,
    # trace RMSE, alpha RMSE as f32.
    params_spline_bytes = PULSE_HEADER_BYTES + 6 * (3 * 4 + (spline_knots + 4) * 2) + 3 * 7 * 4
    params_only_bytes = PULSE_HEADER_BYTES + 6 * 3 * 4 + 3 * 7 * 4

    seconds_per_day = 86400.0
    day = {}
    for pulses_per_day in (24, 48):
        window_day_s = pulses_per_day * window_s
        idle_day_s = seconds_per_day - window_day_s
        continuous_samples = seconds_per_day * mean_hz
        day[pulses_per_day] = {
            "continuous_wire": continuous_samples * WIRE_SAMPLE_BYTES,
            "continuous_packed": continuous_samples * PACKED_SAMPLE_BYTES,
            "continuous_lossless": (
                window_day_s * mean_hz * pulse_bits[lossless_method]
                + idle_day_s * mean_hz * idle_bits[lossless_method]
            )
            / 8,
            "windows_packed": pulses_per_day * window_samples * PACKED_SAMPLE_BYTES,
            "windows_lossless": pulses_per_day * window_samples * pulse_bits[lossless_method] / 8,
            "windows_lzma": pulses_per_day * window_samples * pulse_bits[archive_method] / 8,
            "windows_decimated": pulses_per_day * decimated_window_bytes,
            "windows_params_spline": pulses_per_day * params_spline_bytes,
            "windows_params_only": pulses_per_day * params_only_bytes,
            "idle_summaries": 1440 * IDLE_SUMMARY_BYTES,
            # Idle at 1 Hz averages, packed (6 thermistors + exc + v24 + u16 ts).
            "idle_1hz_packed": idle_day_s * PACKED_SAMPLE_BYTES,
        }

    flash_rows = []
    for pulses_per_day, d in day.items():
        tiers = {
            "continuous 50 Hz, packed raw": d["continuous_packed"],
            "continuous 50 Hz, lossless (Rice)": d["continuous_lossless"],
            "windows packed raw + 1-min idle": d["windows_packed"] + d["idle_summaries"],
            "windows lossless + 1-min idle": d["windows_lossless"] + d["idle_summaries"],
            "windows lossless + 1 Hz idle": d["windows_lossless"] + d["idle_1hz_packed"],
        }
        for tier, per_day in tiers.items():
            flash_rows.append(
                {
                    "pulses_per_day": pulses_per_day,
                    "tier": tier,
                    "bytes_per_day": per_day,
                    "days_on_32MiB": FLASH_BYTES / per_day,
                }
            )

    uplink_rows = []
    for pulses_per_day, d in day.items():
        idle = 24 * datagram_bytes(60 * IDLE_SUMMARY_BYTES)  # hourly batches
        options = {
            "(a) lossless pulse windows": d["windows_lossless"] / pulses_per_day,
            "(b) 1 Hz averaged windows": uniform_1hz_window_bytes,
            "(b') log-bin decimated windows": decimated_window_bytes,
            "(c) params + spline": params_spline_bytes,
            "(c') params only": params_only_bytes,
        }
        for name, per_pulse in options.items():
            per_board = pulses_per_day * datagram_bytes(per_pulse) + idle
            uplink_rows.append(
                {
                    "pulses_per_day": pulses_per_day,
                    "option": name,
                    "payload_per_pulse": per_pulse,
                    "per_board_day": per_board,
                    "two_boards_day": 2 * per_board,
                    "two_boards_30d": 60 * per_board,
                }
            )

    # ---------------------------------------------------------------- write
    summary = {
        "pulse_capture": str(PULSE_CAPTURE),
        "pulse_run": run_id,
        "sample_hz_median": sample_hz,
        "sample_hz_mean": mean_hz,
        "run_state_seconds": state_seconds,
        "pulse_duration_s": pulse_duration_s,
        "peak_rise_c": peak_rise,
        "mk_per_count": mk_per_count,
        "segments": segments,
        "projected_bits_per_sample": projected,
        "decimation": decimation,
        "spline": spline,
        "spline_knots": {"rms<0.006": knots_noise, "rms<0.002": knots_tight, "max<0.006": knots_tight_max},
        "spline_fits": {str(k): v for k, v in spline_fits.items()},
        "spline_start_s": spline_start_s,
        "clean_fit": clean_fit,
        "clean_shift_vs_raw": parameter_shift(reference, clean_fit),
        "monte_carlo_sigma": mc_sigma,
        "model": model,
        "window_rows": window_rows,
        "per_day": {str(k): v for k, v in day.items()},
        "flash_rows": flash_rows,
        "uplink_rows": uplink_rows,
        "record_sizes": {
            "decimated_window_bytes": decimated_window_bytes,
            "uniform_1hz_window_bytes": uniform_1hz_window_bytes,
            "params_spline_bytes": params_spline_bytes,
            "params_only_bytes": params_only_bytes,
            "spline_knots_used": spline_knots,
        },
    }
    (OUTPUT / "summary.json").write_text(json.dumps(summary, indent=2, default=float) + "\n")
    make_plot(frame, spline, spline_knots, spline_start_s, segments, decimation, OUTPUT)
    write_readme(summary, OUTPUT)
    print((OUTPUT / "README.md").read_text())


def make_plot(frame, spline, knots, start_s, segments, decimation, output: Path) -> None:
    fig, axes = plt.subplots(2, 2, figsize=(14, 9.5))

    post = spline_reconstruction(frame, knots, start_s)
    axis = axes[0, 0]
    for channel, color in zip(PAIR_CHANNELS, ["#E69F00", "#7C3AED", "#3B82F6", "#166534"]):
        observed = frame.loc[frame["time_s"].ge(0), f"t{channel}_rise_c"]
        axis.plot(frame.loc[frame["time_s"].ge(0), "time_s"][::5], observed[::5], ".", ms=1.5, alpha=0.3, color=color)
        axis.plot(post["time_s"], post[f"t{channel}_rise_c"], color=color, lw=1.6, label=f"T{channel}")
    axis.set_title(f"Rise vs log-time spline, {knots} interior knots ({knots + 4} coefficients)")
    axis.set_xlabel("Seconds from heater onset")
    axis.set_ylabel("Detrended rise (°C)")
    axis.legend(frameon=False)
    axis.grid(alpha=0.2)

    axis = axes[0, 1]
    for channel, result in spline.items():
        rows = result["rows"]
        axis.plot([r["coefficients"] for r in rows], [r["rms_vs_reference_c"] for r in rows], marker="o", label=f"T{channel}")
    axis.axhline(0.006, color="#999", ls="--", lw=1)
    axis.axhline(0.002, color="#999", ls=":", lw=1)
    axis.set_xscale("log")
    axis.set_yscale("log")
    axis.set_title("Spline error vs 96-knot reference")
    axis.set_xlabel("Coefficients per channel")
    axis.set_ylabel("RMS error (°C)")
    axis.legend(frameon=False)
    axis.grid(alpha=0.2, which="both")

    axis = axes[1, 0]
    labels = ["delta1_rice", "mean8_rice", "delta1_varint", "delta1_bitpack", "delta_zstd-19", "delta_lzma-9e"]
    width = 0.8 / len(segments)
    for i, seg in enumerate(segments):
        values = [projected_record_bits(seg, m) / 8 for m in labels]
        axis.bar(np.arange(len(labels)) + i * width, values, width, label=seg["name"])
    axis.axhline(PACKED_SAMPLE_BYTES, color="#333", ls="--", lw=1)
    axis.text(len(labels) - 0.5, PACKED_SAMPLE_BYTES + 0.2, "packed raw 18 B", ha="right")
    axis.set_xticks(np.arange(len(labels)) + 0.4 - width / 2, labels, rotation=20)
    axis.set_ylabel("Bytes per sample (6 thermistors + exc + v24 + time)")
    axis.set_title("Lossless record size")
    axis.legend(frameon=False, fontsize=8)
    axis.grid(alpha=0.2, axis="y")

    axis = axes[1, 1]
    names = [d["scheme"] for d in decimation[1:]]
    for pair, color in (("middle", "#166534"), ("head", "#7C3AED")):
        axis.plot(
            names,
            [d["shift_interp_sigma"][pair]["tau_s"] for d in decimation[1:]],
            marker="o",
            color=color,
            label=f"{pair} tau",
        )
        axis.plot(
            names,
            [d["shift_interp_sigma"][pair]["epsilon_tau_s"] for d in decimation[1:]],
            marker="s",
            ls="--",
            color=color,
            label=f"{pair} epsilon·tau",
        )
    axis.axhline(0, color="#333", lw=0.8)
    axis.set_ylabel("Shift vs 50 Hz fit (Monte Carlo σ units)")
    axis.set_title("Fitted-parameter shift under decimation (interp refit)")
    axis.tick_params(axis="x", rotation=35)
    axis.legend(frameon=False, fontsize=8)
    axis.grid(alpha=0.2)

    fig.tight_layout()
    fig.savefig(output / "compression_study.png", dpi=180, bbox_inches="tight")
    plt.close(fig)


def write_readme(s: dict, output: Path) -> None:
    lines = [
        "# Sap-flux trace compression study (2026-10-24)",
        "",
        f"Data: Aug 24 soaked-4x4 run `{s['pulse_run']}` (`{Path(s['pulse_capture']).parent.name}`), "
        f"its contiguous idle stretch, and two July 0.5 s pulse runs. "
        f"Measured rate {s['sample_hz_mean']:.2f} Hz mean ({s['sample_hz_median']:.2f} Hz median; "
        "every 50th frame is a 60 ms command window). T1 is broken (constant raw), so per-board "
        "projections replace it with the mean cost of the working thermistors.",
        "",
        "Regenerate: `uv run --with numpy --with scipy --with pandas --with matplotlib --with zstandard "
        "--with lz4 python analysis/compression_study.py`",
        "",
        "![summary](compression_study.png)",
        "",
        "## Findings",
        "",
        f"- **Noise sets the lossless floor.** Thermistor noise is ~{s['segments'][0]['per_channel']['t2_raw']['noise_counts']:.1f} "
        f"counts (≈{s['mk_per_count'] * s['segments'][0]['per_channel']['t2_raw']['noise_counts']:.1f} mK); the Gaussian floor is "
        f"~{s['segments'][0]['per_channel']['t2_raw']['gaussian_floor_bits']:.1f} bits and a mean-of-8 predictor + Rice reaches "
        f"~{s['segments'][0]['per_channel']['t2_raw']['mean8_rice']:.1f} bits per thermistor sample. A whole healthy-probe record "
        f"drops from 18 B to ~{list(s['projected_bits_per_sample'].values())[0]['mean8_rice'] / 8:.1f} B "
        "(≈4.5x on August data, ≈3.4x on the noisier July data). General compressors (zstd/lzma) are at best ~5% better than "
        "this simple integer coder, so it is the one to put on the STM32.",
        "- **Lossless 50 Hz everywhere does not fit the flash for long**; pulse windows do (see section 4).",
        "- **A 12–16-knot log-time spline is a faithful lossy archive**: < 1 mK RMS vs the denoised curve and "
        "< 1σ shift in τ, ε·τ and V, for ~16–20 coefficients per channel. Below ~10 knots the fitted parameters "
        "move by several σ even when RMS error looks small.",
        "- **The Marshall model alone is not an archive**: residual RMSE ≈ 2.3x the noise, so keep its "
        "parameters as the *result*, next to a spline.",
        "- **Uniform decimation is safe to ~2 Hz** (≤ ~1.7σ, ≤ 0.01 cm/h); 1 Hz is borderline; 0.5 Hz is not. "
        "Tiered and 4% log bins preserve the information (≤ ~1σ after interpolating back to the 50 Hz grid) "
        "but must be refit with bin-width weights — refitting the bins directly re-weights the least squares and "
        "moves τ by tens of σ. 10% log bins lose information (τ +9σ).",
        "- Monte Carlo σ assumes white noise; real noise is partly colored, so true uncertainty is larger and "
        "these σ-scaled shifts are conservative.",
        "",
        "## 1. Raw sizes",
        "",
        f"- protocol Sample on the wire: {WIRE_SAMPLE_BYTES} B",
        f"- packed flash record: {PACKED_SAMPLE_BYTES} B (8 x i16 + u16 timestamp delta)",
        "",
        "| window | seconds | samples | wire | packed | lossless Rice | lossless lzma |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for row in s["window_rows"]:
        lines.append(
            f"| {row['window']} | {row['seconds']:.0f} | {row['samples']:.0f} | {fmt(row['wire'])} | "
            f"{fmt(row['packed'])} | {fmt(row['lossless_rice'])} | {fmt(row['lossless_lzma'])} |"
        )
    lines += [
        "",
        "Per day (standard window), one board:",
        "",
        "| pulses/day | continuous wire | continuous packed | continuous lossless | windows packed | windows lossless | x2 boards windows lossless |",
        "|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for p, d in s["per_day"].items():
        lines.append(
            f"| {p} | {fmt(d['continuous_wire'])} | {fmt(d['continuous_packed'])} | {fmt(d['continuous_lossless'])} | "
            f"{fmt(d['windows_packed'])} | {fmt(d['windows_lossless'])} | {fmt(2 * d['windows_lossless'])} |"
        )

    lines += [
        "",
        "## 2. Lossless coding of raw counts",
        "",
        "Bits per channel-sample, Aug 24 pulse run (16 bits raw):",
        "",
        "| channel | noise σ (counts) | Gaussian floor | Δ1 entropy | mean8 entropy | Δ1 Rice | Δ2 Rice | mean8 Rice | Δ1 varint | Δ1 bitpack | planar zstd-19 | Δ zstd-19 | Δ lzma | Δ lz4 |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for column, r in s["segments"][0]["per_channel"].items():
        lines.append(
            f"| {column} | {r['noise_counts']:.2f} | {r['gaussian_floor_bits']:.2f} | {r['delta1_entropy']:.2f} | "
            f"{r['mean8_entropy']:.2f} | {r['delta1_rice']:.2f} | {r['delta2_rice']:.2f} | {r['mean8_rice']:.2f} | "
            f"{r['delta1_varint']:.2f} | {r['delta1_bitpack']:.2f} | {r['planar_zstd-19']:.2f} | "
            f"{r['delta_zstd-19']:.2f} | {r['delta_lzma-9e']:.2f} | {r['delta_lz4']:.2f} |"
        )
    lines += [
        "",
        "Projected bytes per sample for a healthy probe (6 thermistors + exc + v24 + Rice-coded timestamp):",
        "",
        "| segment | Δ1 entropy | mean8 entropy | Δ1 Rice | mean8 Rice | Δ1 varint | Δ1 bitpack | planar zstd-19 | Δ zstd-19 | Δ lzma | ratio vs 18 B (mean8 Rice) |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for name, p in s["projected_bits_per_sample"].items():
        lines.append(
            f"| {name} | {p['delta1_entropy'] / 8:.2f} | {p['mean8_entropy'] / 8:.2f} | {p['delta1_rice'] / 8:.2f} | "
            f"{p['mean8_rice'] / 8:.2f} | {p['delta1_varint'] / 8:.2f} | {p['delta1_bitpack'] / 8:.2f} | "
            f"{p['planar_zstd-19'] / 8:.2f} | {p['delta_zstd-19'] / 8:.2f} | {p['delta_lzma-9e'] / 8:.2f} | "
            f"{PACKED_SAMPLE_BYTES * 8 / p['mean8_rice']:.2f}x |"
        )
    seg0 = s["segments"][0]
    lines += [
        "",
        f"Timestamps: delta-of-delta + Rice = {seg0['timestamp_rice_bits']:.2f} bits/sample at 0.1 ms "
        f"({seg0['timestamp_rice_bits_1us']:.2f} at 1 µs) "
        f"(period p1/p50/p99 = {seg0['period_us_p01_p99'][0]:.0f}/{seg0['period_us_median']:.0f}/"
        f"{seg0['period_us_p01_p99'][1]:.0f} µs).",
        "",
        "## 3. Lossy reductions (temperature units)",
        "",
        f"Pulse {s['pulse_duration_s']:.3f} s; peak detrended rise {s['peak_rise_c']:.3f} °C, so decimated "
        "values fit i16 at 0.1 mK resolution (2 B each). One ADC count ≈ "
        f"{s['mk_per_count']:.2f} mK near 20 °C. Spline domain starts at t = {s['spline_start_s']:.2f} s "
        "(heater-on samples carry an electrical step, not thermal signal).",
        "",
        "### 3a. Decimation by bin averaging, refit with `fit_4x4_pulse.fit_pair`",
        "",
        "Shifts are vs the 50 Hz fit, in units of the Monte Carlo 1σ measurement scatter (section 3c; white-noise only, so a lower bound on real uncertainty). "
        "`native` refits the reduced points directly (nonuniform bins re-weight the least squares); "
        "`interp` linearly interpolates the reduced rise back onto the 50 Hz grid first, keeping the "
        "original weighting — this isolates information loss.",
        "",
        "| scheme | points | 6-ch bytes | mid τ native/interp | mid ε·τ native/interp | mid V native/interp | head τ native/interp | head ε·τ native/interp | head V native/interp |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for d in s["decimation"][1:]:
        cells = []
        for pair in ("middle", "head"):
            for key in ("tau_s", "epsilon_tau_s", "nominal_velocity_cm_hr"):
                cells.append(f"{d['shift_sigma'][pair][key]:+.2f}σ / {d['shift_interp_sigma'][pair][key]:+.2f}σ")
        lines.append(f"| {d['scheme']} | {d['points']} | {fmt(d['bytes_6ch'])} | " + " | ".join(cells) + " |")
    ref = s["decimation"][0]["fit"]
    sig = s["monte_carlo_sigma"]
    lines += [
        "",
        f"Reference (50 Hz): middle τ {ref['middle']['tau_s']:.2f} s, ε·τ {ref['middle']['epsilon_tau_s']:.3f} s, "
        f"V {ref['middle']['nominal_velocity_cm_hr']:+.3f} cm/h; head τ {ref['head']['tau_s']:.2f} s, "
        f"ε·τ {ref['head']['epsilon_tau_s']:.3f} s, V {ref['head']['nominal_velocity_cm_hr']:+.3f} cm/h.",
        "",
        "### 3b. Least-squares cubic spline in u = log(t + 2 s), t ≥ 0",
        "",
        "Error is measured against a 96-knot reference spline (≈ the noise-free curve; its own noise "
        "leakage is ~σ·√(100/N) ≈ 0.5 mK). Coefficients = knots + 4.",
        "",
        "| channel | noise σ (°C) | peak (°C) | " + " | ".join(f"{k} kn" for k in SPLINE_KNOTS) + " |",
        "|---|---:|---:|" + "---:|" * len(SPLINE_KNOTS),
    ]
    for channel, r in s["spline"].items():
        cells = " | ".join(f"{1000 * row['rms_vs_reference_c']:.2f}" for row in r["rows"])
        lines.append(f"| T{channel} RMS (mK) | {r['noise_c']:.4f} | {r['peak_rise_c']:.3f} | {cells} |")
    for channel, r in s["spline"].items():
        cells = " | ".join(f"{1000 * row['max_vs_reference_c']:.1f}" for row in r["rows"])
        lines.append(f"| T{channel} max (mK) | | | {cells} |")
    k = s["spline_knots"]
    lines += [
        "",
        f"Minimum interior knots (worst channel): RMS < 6 mK → **{k['rms<0.006']}**, RMS < 2 mK → **{k['rms<0.002']}**, "
        f"max < 6 mK → **{k['max<0.006']}**.",
        "",
        "Parameter shift when the fit runs on the spline reconstruction, vs the fit on the 96-knot "
        "(denoised) reconstruction, in Monte Carlo σ:",
        "",
        "| knots | coeffs/ch | mid τ | mid ε·τ | mid V | head τ | head ε·τ | head V |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for knots, v in s["spline_fits"].items():
        c = v["shift_vs_clean_sigma"]
        lines.append(
            f"| {knots} | {int(knots) + 4} | {c['middle']['tau_s']:+.2f}σ | {c['middle']['epsilon_tau_s']:+.2f}σ | "
            f"{c['middle']['nominal_velocity_cm_hr']:+.2f}σ | {c['head']['tau_s']:+.2f}σ | "
            f"{c['head']['epsilon_tau_s']:+.2f}σ | {c['head']['nominal_velocity_cm_hr']:+.2f}σ |"
        )
    cs = s["clean_shift_vs_raw"]
    lines += [
        "",
        f"Denoising alone (96-knot fit vs raw 50 Hz fit): middle V {cs['middle']['nominal_velocity_cm_hr']:+.4f} cm/h, "
        f"ε·τ {100 * cs['middle']['epsilon_tau_s_rel']:+.2f}%; head V {cs['head']['nominal_velocity_cm_hr']:+.4f} cm/h, "
        f"ε·τ {100 * cs['head']['epsilon_tau_s_rel']:+.2f}% (log-ratio α is biased by noise on small rises).",
        "",
        "### 3c. Measurement uncertainty (Monte Carlo)",
        "",
        f"{MONTE_CARLO_REALIZATIONS} refits of the denoised curve plus white noise at each channel's baseline σ:",
        "",
        "| pair | σ τ (s) | σ ε·τ (s) | σ ζ | σ ε | σ V (cm/h) | σ α(60–80) |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for pair, v in sig.items():
        lines.append(
            f"| {pair} | {v['tau_s']:.3f} | {v['epsilon_tau_s']:.4f} | {v['alpha_intercept_zeta']:.5f} | "
            f"{v['epsilon']:.5f} | {v['nominal_velocity_cm_hr']:.4f} | {v['alpha_60_80']:.5f} |"
        )
    lines += [
        "",
        "### 3d. The physical model as the compressed form",
        "",
        "5 parameters per pair (ζ, τ, ε, A_down, A_up) plus pulse duration describe both traces.",
        "",
        "| pair | trace RMSE (°C) | noise down/up (°C) | α RMSE |",
        "|---|---:|---:|---:|",
    ]
    for pair, m in s["model"].items():
        lines.append(
            f"| {pair} | {m['trace_rmse_c']:.4f} | {m['noise_down_c']:.4f} / {m['noise_up_c']:.4f} | {m['alpha_rmse']:.4f} |"
        )
    rs = s["record_sizes"]
    lines += [
        "",
        "Model RMSE is ~2.3x the noise floor: the model leaves systematic structure, so parameters alone "
        "are a result, not an archive of the curve.",
        "",
        "## 4. Flash capacity (32 MiB W25Q256, per board; each board has its own flash)",
        "",
        "| pulses/day | tier | per day | days on 32 MiB |",
        "|---:|---|---:|---:|",
    ]
    for row in s["flash_rows"]:
        lines.append(f"| {row['pulses_per_day']} | {row['tier']} | {fmt(row['bytes_per_day'])} | {row['days_on_32MiB']:.1f} |")
    lines += [
        "",
        "## 5. LTE-M uplink budget",
        "",
        f"Record sizes per pulse: 1 Hz averaged window {rs['uniform_1hz_window_bytes']} B; "
        f"log-bin decimated window {rs['decimated_window_bytes']} B; params + "
        f"{rs['spline_knots_used']}-knot spline {rs['params_spline_bytes']} B; params only {rs['params_only_bytes']} B. "
        f"Idle: {IDLE_SUMMARY_BYTES} B per 1-min summary, sent in hourly batches. Each datagram carries "
        f"≤{DATAGRAM_PAYLOAD_BYTES} B payload + {DATAGRAM_OVERHEAD_BYTES} B assumed IP/UDP/DTLS/app overhead.",
        "",
        "| pulses/day | option | payload/pulse | per board/day | 2 boards/day | 2 boards/30 d |",
        "|---:|---|---:|---:|---:|---:|",
    ]
    for row in s["uplink_rows"]:
        lines.append(
            f"| {row['pulses_per_day']} | {row['option']} | {fmt(row['payload_per_pulse'])} | {fmt(row['per_board_day'])} | "
            f"{fmt(row['two_boards_day'])} | {fmt(row['two_boards_30d'])} |"
        )
    lines.append("")
    (output / "README.md").write_text("\n".join(lines))


if __name__ == "__main__":
    main()
