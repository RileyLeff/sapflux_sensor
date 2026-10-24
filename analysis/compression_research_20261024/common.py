"""Shared corpus, conversion and science harness for the compression research.

Everything here works on one *window*: 13 channels of raw ADS1115 counts
(excitation + 12 thermistors, probe 1 T1..T6 then probe 2 T1..T6) sampled
at 25 Hz from 60 s before heater onset to 181 s after, as the field board
will record it.  Encoders take a Window and return a reconstruction of the
raw counts (float) on the same time grid, plus a byte count.  The science
harness then converts to temperature and runs every downstream estimator the
scientists might want (Marshall finite-pulse fit, heat-ratio windows, Tmax,
peak rise, baseline), so fidelity is judged on the science, not on RMS.

Corpus:

* three real pulses (two usable): Aug 24 soaked 4x4 (1 s, ~1.3 °C), Jul 11
  bench run 1783745735 (0.5 s, ~0.7 °C, only 30 s of baseline) and Jul 11
  run 1783743736 (0.5 s, 0.04 °C rise — a "null" pulse, used for anomaly
  checks only);
* a semi-synthetic corpus: Marshall finite-pulse signals with realistic
  parameters, plus the systematic model misfit measured on the real Aug
  pulse, plus a diurnal-style baseline drift, added to *real recorded idle
  windows* (so the noise is the board's real colored noise, including the
  9.6 Hz interference seen in two sessions).
"""

from __future__ import annotations

import argparse
import functools
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import pandas as pd
from scipy.interpolate import LSQUnivariateSpline
from scipy.signal import savgol_filter

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import fit_4x4_pulse as fit  # noqa: E402

CAPTURES = HERE.parent.parent / "software/sap-flux-monitor/captures"
CACHE = HERE / "cache"  # gitignored; holds the generated corpus
FS = 25.0
PRE_S = 60.0
POST_S = 181.0
PULSE_S = 1.0
R_REF = 1e4
STEINHART = (0.003354, 0.0002565, 2.325e-6, 1.024e-7)
N_CH = 13  # index 0 = excitation, 1..12 thermistors
EXC = 0

# Pairs on a 12-thermistor board, as (downstream, upstream) channel indices
# into the 13-channel array.  Probe p (0/1) thermistor Tk -> 1 + 6p + (k-1).
# Field wiring follows the Aug sensor head: T1/T4 tip, T2/T5 middle, T3/T6 head.
FIELD_PAIRS = {
    f"p{p + 1}_{name}": (1 + 6 * p + d - 1, 1 + 6 * p + u - 1)
    for p in range(2)
    for name, (d, u) in {"tip": (1, 4), "middle": (2, 5), "head": (3, 6)}.items()
}

ALPHA_WINDOWS = [(30.0, 50.0), (60.0, 80.0), (90.0, 110.0)]


# --------------------------------------------------------------------------
# Thermistor conversion


def raw_to_temp(raw: np.ndarray, exc: np.ndarray) -> np.ndarray:
    raw = np.asarray(raw, float)
    exc = np.asarray(exc, float)
    with np.errstate(invalid="ignore", divide="ignore"):
        r = R_REF * (exc - raw) / raw
        lnr = np.log(r / R_REF)
    c0, c1, c2, c3 = STEINHART
    return 1.0 / (c0 + c1 * lnr + c2 * lnr**2 + c3 * lnr**3) - 273.15


def temp_to_resistance(temp_c: np.ndarray) -> np.ndarray:
    """Invert the Steinhart-Hart cubic numerically (Newton on ln R)."""
    target = 1.0 / (np.asarray(temp_c, float) + 273.15)
    c0, c1, c2, c3 = STEINHART
    lnr = np.zeros_like(target)
    for _ in range(30):
        g = c0 + c1 * lnr + c2 * lnr**2 + c3 * lnr**3 - target
        dg = c1 + 2 * c2 * lnr + 3 * c3 * lnr**2
        lnr -= g / dg
    return R_REF * np.exp(lnr)


def temp_to_raw(temp_c: np.ndarray, exc: np.ndarray) -> np.ndarray:
    r = temp_to_resistance(temp_c)
    return np.asarray(exc, float) * R_REF / (r + R_REF)


def smooth_excitation(exc: np.ndarray, mode: str = "window_line", t: np.ndarray | None = None) -> np.ndarray:
    """Excitation estimate used in the ratiometric conversion.

    The excitation channel's sample-to-sample scatter is mostly ADC noise,
    not real excitation change (measured: thermistor counts follow only
    ~40 % of the ideal ratiometric slope), so smoothing it lowers the
    temperature noise.  See exp_noise.py."""
    exc = np.asarray(exc, float)
    if mode == "per_sample":
        return exc
    if mode == "mean":
        return np.full_like(exc, np.nanmean(exc))
    if mode == "window_line":
        x = np.arange(len(exc)) if t is None else t
        ok = np.isfinite(exc)
        p = np.polyfit(x[ok], exc[ok], 1)
        return np.polyval(p, x)
    if mode.startswith("ma"):
        from scipy.ndimage import uniform_filter1d

        n = int(mode[2:])
        return uniform_filter1d(exc, n, mode="nearest")
    raise ValueError(mode)


EXC_MODE = "ma13"  # 0.5 s boxcar: as good as per-sample, see exp_noise.py


# --------------------------------------------------------------------------
# Windows


@dataclass
class Window:
    name: str
    kind: str  # "real", "synthetic", "null"
    regime: str  # noise regime: "quiet", "interference", "july"
    t: np.ndarray  # seconds from heater onset, length N
    raw: np.ndarray  # (N, 13) float counts (integers for real/synthetic)
    pairs: dict  # pair name -> (down, up) channel index
    pulse_s: float
    live: list  # thermistor channel indices that are alive
    truth: dict = field(default_factory=dict)  # synthetic truth params
    group: int = -1  # synthetic signal-set id (for Monte Carlo sigma)

    @property
    def n(self) -> int:
        return len(self.t)


def _contiguous_blocks(t_us: np.ndarray, max_gap_us: int = 200_000):
    d = np.diff(t_us)
    br = np.flatnonzero((d <= 0) | (d > max_gap_us)) + 1
    e = np.concatenate(([0], br, [len(t_us)]))
    return [(int(a), int(b)) for a, b in zip(e[:-1], e[1:])]


RAW_COLS = ["excitation_raw"] + [f"t{i}_raw" for i in range(1, 7)]


@functools.lru_cache(maxsize=None)
def _read(session: str) -> pd.DataFrame:
    return pd.read_csv(CAPTURES / session / "samples.csv")


def real_pulse(session: str, run_id: int, pairs6: dict, live6: list, kind: str, regime: str, pre_s: float = PRE_S) -> Window:
    """A real 6-thermistor pulse, decimated 50 -> 25 Hz by taking every
    second frame (the field ADS1115 runs the same data rate per conversion),
    placed in probe-1 slots of a 13-channel array; probe-2 slots are NaN."""
    f = _read(session)
    on = int(np.flatnonzero(f["run_id"].eq(run_id).to_numpy() & f["heater_active"].to_numpy(bool))[0])
    t_us = f["monotonic_us"].to_numpy(np.int64)
    # the run id stops at the end of the configured recovery; keep the whole
    # contiguous block around the onset instead
    a, b = next(ab for ab in _contiguous_blocks(t_us) if ab[0] <= on < ab[1])
    f = f.iloc[a:b].reset_index(drop=True)
    on -= a
    t_us = t_us[a:b]
    t = (t_us - t_us[on]) / 1e6
    # keep onset sample on the grid: take every second frame aligned to onset
    idx = np.arange(on % 2, len(f), 2)
    idx = idx[(t[idx] >= -pre_s) & (t[idx] <= POST_S)]
    raw = np.full((len(idx), N_CH), np.nan)
    sub = f.iloc[idx]
    raw[:, 0] = sub["excitation_raw"].to_numpy(float)
    for k in range(1, 7):
        raw[:, k] = sub[f"t{k}_raw"].to_numpy(float)
    heat = f["heater_active"].to_numpy(bool)
    dur = float((t_us[np.flatnonzero(heat)[-1]] - t_us[on]) / 1e6 + np.median(np.diff(t_us)) / 1e6)
    pairs = {f"p1_{k}": (v[0], v[1]) for k, v in pairs6.items()}
    return Window(
        name=f"{session}:{run_id}", kind=kind, regime=regime, t=t[idx], raw=raw,
        pairs=pairs, pulse_s=dur, live=list(live6),
    )


def real_pulses() -> list[Window]:
    aug = real_pulse(
        "session_20260824_203902", 1787618451,
        {"middle": (2, 5), "head": (3, 6)}, [2, 3, 4, 5, 6], "real", "quiet",
    )
    jul = real_pulse(
        "session_20260711_005525", 1783745735,
        {"head": (1, 4), "middle": (2, 5)}, [1, 2, 4, 5, 6], "real", "july", pre_s=30.0,
    )
    null = real_pulse(
        "session_20260711_002159", 1783743736,
        {"head": (1, 4), "middle": (2, 5)}, [1, 2, 4, 5, 6], "null", "july",
    )
    aug.name, jul.name, null.name = "real_aug24_soaked4x4", "real_jul11_bench", "null_jul11_norise"
    return [aug, jul, null]


# Idle stretches used as real noise (start/end in seconds from the start of
# the longest contiguous block; Aug 203902 starts 600 s after its pulse).
IDLE_SOURCES = [
    ("session_20260824_203902", "quiet", "after_pulse", [2, 3, 4, 5, 6]),
    ("session_20260824_212832", "interference", "all", [2, 3, 4, 5, 6]),
    ("session_20260711_003122", "july", "all", [1, 2, 4, 5, 6]),
]


def idle_blocks() -> list[tuple[str, str, np.ndarray, list]]:
    """Return (session, regime, raw (N,7) at 25 Hz, live) for each idle source."""
    out = []
    for session, regime, which, live in IDLE_SOURCES:
        f = _read(session)
        if which == "after_pulse":
            heat = np.flatnonzero(f["heater_active"].to_numpy(bool))
            f = f.iloc[heat[-1] + int(600 * 49.8):]
        t_us = f["monotonic_us"].to_numpy(np.int64)
        a, b = max(_contiguous_blocks(t_us), key=lambda ab: ab[1] - ab[0])
        g = f.iloc[a:b:2]
        out.append((session, regime, g[RAW_COLS].to_numpy(float), live))
    return out


def noise_windows(n_samples: int, stride_s: float = 60.0) -> list[dict]:
    """Slice idle blocks into overlapping windows of n_samples (25 Hz)."""
    wins = []
    for session, regime, raw, live in idle_blocks():
        stride = int(stride_s * FS)
        for start in range(0, len(raw) - n_samples + 1, stride):
            wins.append({"session": session, "regime": regime, "start": start,
                         "raw": raw[start:start + n_samples], "live": live})
    return wins


# --------------------------------------------------------------------------
# Synthetic corpus


def marshall_rise(t: np.ndarray, zeta: float, tau: float, eps: float, side: int, dur: float) -> np.ndarray:
    out = np.zeros_like(t)
    m = t > 0
    out[m] = fit.finite_pulse_response(t[m], zeta, tau, eps, side, dur)
    return out


@functools.lru_cache(maxsize=1)
def misfit_templates() -> list[np.ndarray]:
    """Systematic (data - Marshall model) residual of the real Aug pulse,
    as functions of log time, normalized by each channel's peak rise.

    Returned as samples on a fixed log-time grid u = log(t + 2), t in
    [0, 181]; template 0 before 1.2 s (heater-on)."""
    args = fit_args()
    w = real_pulses()[0]
    temps = window_temps(w)
    frame = pd.DataFrame({"time_s": w.t})
    for c in w.live:
        frame[f"t{c}_rise_c"] = detrend(w.t, temps[c])
    ug = np.log(np.linspace(0, 181, 2000) + 2.0)
    temps_out = []
    for pair, (d, u) in w.pairs.items():
        res, fitted = fit.fit_pair(frame, pair, d, u, w.pulse_s, args)
        for ch, amp, side in ((d, res.downstream_amplitude_c, 1), (u, res.upstream_amplitude_c, -1)):
            post = w.t >= 1.2
            tt = w.t[post]
            model = amp * marshall_rise(tt, res.alpha_intercept_zeta, res.tau_s, res.epsilon, side, w.pulse_s)
            resid = frame[f"t{ch}_rise_c"].to_numpy()[post] - model
            uu = np.log(tt + 2.0)
            knots = np.linspace(uu[0], uu[-1], 26)[1:-1]
            s = LSQUnivariateSpline(uu, resid, knots, k=3)
            peak = float(np.max(frame[f"t{ch}_rise_c"].to_numpy()[post]))
            tpl = s(ug) / peak
            tpl[np.exp(ug) - 2.0 < 1.2] = 0.0
            temps_out.append(tpl)
    return temps_out


def synth_signal_sets(n_sets: int, seed: int = 7) -> list[dict]:
    """Random physical parameters for one 12-thermistor window."""
    rng = np.random.default_rng(seed)
    sets = []
    while len(sets) < n_sets:
        k = rng.uniform(0.0020, 0.0030)  # cm^2/s
        xbar = 0.6
        tau = xbar**2 / k
        v_outer = rng.uniform(-1.0, 22.0)  # cm/h, outer depth
        depth_mult = {"tip": rng.uniform(0.2, 0.6), "middle": rng.uniform(0.5, 0.9), "head": 1.0}
        pairs = {}
        ok = True
        for pname in FIELD_PAIRS:
            depth = pname.split("_")[1]
            v = v_outer * depth_mult[depth] + rng.normal(0, 0.5)
            zeta = v * tau / (3600.0 * xbar)
            eps = rng.uniform(-0.06, 0.06)
            peak = rng.uniform(0.5, 1.8)
            gain_u = rng.uniform(0.9, 1.1)
            tt = np.linspace(0.05, 181, 4000)
            rd = marshall_rise(tt, zeta, tau, eps, 1, PULSE_S)
            ru = marshall_rise(tt, zeta, tau, eps, -1, PULSE_S)
            scale = peak / max(rd.max(), ru.max())
            m = (tt > 8) & (tt < 120)
            if min((scale * rd[m]).max(), (scale * gain_u * ru[m]).max()) < 0.15:
                ok = False
                break
            pairs[pname] = {
                "v_cm_h": v, "zeta": zeta, "tau_s": tau, "epsilon": eps,
                "epsilon_tau_s": eps * tau, "amp_down": scale, "amp_up": scale * gain_u,
                "misfit": [int(rng.integers(0, 8)), float(rng.choice([-1, 1]) * rng.uniform(0.5, 1.5)),
                           int(rng.integers(0, 8)), float(rng.choice([-1, 1]) * rng.uniform(0.5, 1.5))],
            }
        if not ok:
            continue
        sets.append({
            "k_cm2_s": k, "tau_s": tau, "pairs": pairs,
            "drift_mK_min": float(rng.uniform(-40, 40)),
            "drift_curv_mK_min2": float(rng.uniform(-2, 2)),
            "base_temp_c": float(rng.uniform(5, 30)),
        })
    return sets


def synth_window(sig: dict, noise_a: dict, noise_b: dict, name: str, group: int) -> Window:
    """Signal set + two real idle noise windows (probe 1 and probe 2)."""
    n = len(noise_a["raw"])
    t = np.arange(n) / FS - PRE_S
    exc = noise_a["raw"][:, 0]
    tpl = misfit_templates()
    ug = np.log(np.linspace(0, 181, 2000) + 2.0)
    raw = np.zeros((n, N_CH))
    raw[:, 0] = exc
    clean = np.zeros((n, N_CH))  # noise-free twin: same signal, no idle noise
    exc_mean = float(np.mean(exc))
    clean[:, 0] = exc_mean
    tmin = t / 60.0
    drift = 1e-3 * (sig["drift_mK_min"] * tmin + 0.5 * sig["drift_curv_mK_min2"] * tmin**2)
    for p, nz in ((0, noise_a), (1, noise_b)):
        live = nz["live"]
        # broken T1/T3 in a noise source: borrow a live channel from the same window
        for k in range(1, 7):
            src = k if k in live else live[(k * 7) % len(live)]
            idle = nz["raw"][:, src]
            ch = 1 + 6 * p + k - 1
            idle_t = raw_to_temp(idle, exc)
            rise = np.zeros(n)
            for pname, (d, u) in FIELD_PAIRS.items():
                pp = sig["pairs"][pname]
                for chan, side, amp, (ti, ts) in ((d, 1, pp["amp_down"], pp["misfit"][:2]), (u, -1, pp["amp_up"], pp["misfit"][2:])):
                    if chan != ch:
                        continue
                    base = amp * marshall_rise(t, pp["zeta"], pp["tau_s"], pp["epsilon"], side, PULSE_S)
                    mis = np.interp(np.log(np.clip(t, 0, None) + 2.0), ug, tpl[ti % len(tpl)]) * ts * base.max()
                    mis[t < 0] = 0.0
                    rise = base + mis
            offset = sig["base_temp_c"] - np.nanmean(idle_t) + 0.3 * (k - 3) + 0.2 * p
            target_t = idle_t + offset + drift + rise
            # keep the idle channel's own count noise: shift its counts by the
            # count change that moves it from its idle temperature to the target
            raw[:, ch] = np.round(idle + temp_to_raw(target_t, exc) - temp_to_raw(idle_t, exc))
            clean[:, ch] = temp_to_raw(np.nanmean(idle_t) + offset + drift + rise, np.full(n, exc_mean))
    truth = {pn: {k: v for k, v in pp.items() if k != "misfit"} for pn, pp in sig["pairs"].items()}
    truth["clean_raw"] = clean
    return Window(name=name, kind="synthetic", regime=noise_a["regime"], t=t, raw=raw,
                  pairs=dict(FIELD_PAIRS), pulse_s=PULSE_S, live=list(range(1, 13)), truth=truth, group=group)


def build_corpus(n_sets: int = 12, per_set: int = 8, seed: int = 11) -> list[Window]:
    """n_sets signal sets x per_set noise draws each (for per-set sigma)."""
    n = int(round((PRE_S + POST_S) * FS)) + 1
    wins = noise_windows(n)
    rng = np.random.default_rng(seed)
    by_regime = {}
    for w in wins:
        by_regime.setdefault(w["regime"], []).append(w)
    sets = synth_signal_sets(n_sets)
    corpus = []
    for g, sig in enumerate(sets):
        # alternate regimes across signal sets: quiet / interference (july too short)
        regime = ["quiet", "interference"][g % 2]
        pool = by_regime[regime]
        for j in range(per_set):
            a = pool[(j * 2 + g) % len(pool)]
            b = pool[(j * 2 + g + len(pool) // 2) % len(pool)]
            corpus.append(synth_window(sig, a, b, f"syn{g:02d}_{j}", g))
    return corpus


def load_corpus(n_sets: int = 12, per_set: int = 8) -> list[Window]:
    """Cached corpus (pickle in cache/, gitignored)."""
    import pickle

    CACHE.mkdir(exist_ok=True)
    path = CACHE / f"corpus_{n_sets}x{per_set}.pkl"
    if path.exists():
        return pickle.loads(path.read_bytes())
    corpus = build_corpus(n_sets, per_set)
    path.write_bytes(pickle.dumps(corpus))
    return corpus


# --------------------------------------------------------------------------
# Science harness


def fit_args() -> argparse.Namespace:
    return argparse.Namespace(
        nominal_xbar_cm=0.6, heater_voltage_v=25.6, heater_resistance_ohm=42.0,
        heater_effective_length_m=0.02, rho_c_j_m3_k=2.5e6, alpha_start_s=8.0,
        fit_end_s=120.0, minimum_rise_c=0.05,
    )


def window_temps(w: Window, raw: np.ndarray | None = None, exc_mode: str = EXC_MODE) -> dict[int, np.ndarray]:
    raw = w.raw if raw is None else raw
    exc = smooth_excitation(raw[:, EXC], exc_mode, w.t)
    return {c: raw_to_temp(raw[:, c], exc) for c in w.live}


def detrend(t: np.ndarray, temp: np.ndarray, lo: float = -55.0, hi: float = -1.0) -> np.ndarray:
    m = (t >= lo) & (t <= hi) & np.isfinite(temp)
    p = np.polyfit(t[m], temp[m], 1)
    return temp - np.polyval(p, t)


def tmax(t: np.ndarray, rise: np.ndarray) -> tuple[float, float]:
    """Time and height of the peak: Savitzky-Golay (4 s, cubic), then a
    parabola through the +-2 s around the maximum."""
    m = (t > 2.0) & (t < 170.0)
    tt, yy = t[m], rise[m]
    win = int(4.0 * FS) | 1
    sm = savgol_filter(yy, win, 3)
    i = int(np.argmax(sm))
    j = (tt > tt[i] - 2.0) & (tt < tt[i] + 2.0)
    if j.sum() >= 5:
        a, b, c = np.polyfit(tt[j] - tt[i], sm[j], 2)
        if a < 0:
            dt = float(np.clip(-b / (2 * a), -2, 2))
            return float(tt[i] + dt), float(c - b * b / (4 * a))
    return float(tt[i]), float(sm[i])


def science(w: Window, raw: np.ndarray | None = None, exc_mode: str = EXC_MODE) -> dict[str, float]:
    """Every estimator, flattened to {"pair.param": value}."""
    temps = window_temps(w, raw, exc_mode)
    t = w.t
    args = fit_args()
    out: dict[str, float] = {}
    frame = pd.DataFrame({"time_s": t})
    for c in w.live:
        frame[f"t{c}_rise_c"] = detrend(t, temps[c], lo=max(-55.0, t[0] + 1.0))
        base = (t >= -30) & (t <= -1)
        out[f"ch{c}.baseline_c"] = float(np.nanmean(temps[c][base]))
    for pair, (d, u) in w.pairs.items():
        try:
            r, _ = fit.fit_pair(frame, pair, d, u, w.pulse_s, args)
            out[f"{pair}.zeta"] = r.alpha_intercept_zeta
            out[f"{pair}.eps_tau"] = r.epsilon_tau_s
            out[f"{pair}.tau"] = r.tau_s
            out[f"{pair}.V"] = r.nominal_velocity_cm_hr
            out[f"{pair}.amp_down"] = r.downstream_amplitude_c
        except Exception:  # noqa: BLE001
            for k in ("zeta", "eps_tau", "tau", "V", "amp_down"):
                out[f"{pair}.{k}"] = float("nan")
        dn = frame[f"t{d}_rise_c"].to_numpy()
        up = frame[f"t{u}_rise_c"].to_numpy()
        for lo, hi in ALPHA_WINDOWS:
            m = (t >= lo) & (t <= hi)
            with np.errstate(invalid="ignore", divide="ignore"):
                out[f"{pair}.hrm{int(lo)}"] = float(np.log(np.mean(dn[m]) / np.mean(up[m])))
        for side, ch in (("down", d), ("up", u)):
            tm, pk = tmax(t, frame[f"t{ch}_rise_c"].to_numpy())
            out[f"{pair}.tmax_{side}"] = tm
            out[f"{pair}.peak_{side}"] = pk
        m = (t >= 150) & (t <= 180)
        out[f"{pair}.tail_down"] = float(np.mean(dn[m]))
    return out


PARAM_KINDS = ["zeta", "eps_tau", "tau", "V", "amp_down", "hrm30", "hrm60", "hrm90",
               "tmax_down", "tmax_up", "peak_down", "peak_up", "tail_down", "baseline_c"]


def kind_of(key: str) -> str:
    return key.split(".", 1)[1]


def save_json(obj, path: Path) -> None:
    def default(o):
        if isinstance(o, (np.floating,)):
            return float(o)
        if isinstance(o, (np.integer,)):
            return int(o)
        if isinstance(o, np.ndarray):
            return o.tolist()
        raise TypeError(type(o))

    path.write_text(json.dumps(obj, indent=1, default=default) + "\n")


def pmap(fn, items, workers: int = 16):
    """Parallel map (process pool); falls back to serial for tiny jobs."""
    from concurrent.futures import ProcessPoolExecutor

    items = list(items)
    if len(items) < 4:
        return [fn(x) for x in items]
    with ProcessPoolExecutor(max_workers=workers) as ex:
        return list(ex.map(fn, items, chunksize=max(1, len(items) // (workers * 4))))


def group_sigma(corpus: list[Window], estimates: list[dict]) -> dict[tuple[int, str], float]:
    """Per (signal-set, key) standard deviation of the raw-data estimates
    across that set's real-noise draws: the measurement uncertainty."""
    out = {}
    groups = sorted({w.group for w in corpus if w.group >= 0})
    for g in groups:
        idx = [i for i, w in enumerate(corpus) if w.group == g]
        keys = estimates[idx[0]].keys()
        for k in keys:
            vals = np.array([estimates[i][k] for i in idx], float)
            vals = vals[np.isfinite(vals)]
            out[(g, k)] = float(np.std(vals, ddof=1)) if len(vals) > 2 else float("nan")
    return out
