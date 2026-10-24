#!/usr/bin/env python3
"""Survey every capture session: heat-pulse events and idle stretches.

For each real (non-simulated) session, find every contiguous heater-on
segment, how much contiguous baseline/recovery surrounds it, the sampling
rate, which thermistors are alive, the baseline noise and the peak rise per
channel. Also list the long idle stretches (no heater within 300 s).

    uv run --with numpy --with pandas python \
        analysis/compression_research_20261024/survey.py

Writes survey.json and survey.md next to this script.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
CAPTURES = HERE.parent.parent / "software/sap-flux-monitor/captures"
MAX_GAP_US = 200_000
THERM = [f"t{i}" for i in range(1, 7)]


def robust_sigma(x: np.ndarray) -> float:
    x = x[np.isfinite(x)]
    if len(x) < 10:
        return float("nan")
    return float(np.median(np.abs(x - np.median(x))) / 0.6744897501960817)


def contiguous_bounds(t_us: np.ndarray, i: int) -> tuple[int, int]:
    """Indices [lo, hi) of the contiguous (no gap > MAX_GAP_US) block holding i."""
    d = np.diff(t_us)
    bad = np.flatnonzero((d <= 0) | (d > MAX_GAP_US))
    lo = 0
    hi = len(t_us)
    before = bad[bad < i]
    after = bad[bad >= i]
    if len(before):
        lo = int(before[-1]) + 1
    if len(after):
        hi = int(after[0]) + 1
    return lo, hi


def survey_session(path: Path) -> dict:
    meta = json.loads((path / "metadata.json").read_text())
    out = {
        "session": path.name,
        "simulated": meta["simulated"],
        "firmware": meta.get("firmware_version"),
        "board": meta.get("board_id_hex"),
        "start": meta["session_start_iso"][:19],
        "channels": {c["id"]: c["position"] for c in meta["channels"]},
        "known_broken": [c["id"] for c in meta["channels"] if c["known_broken"]],
        "pulses": [],
        "idle": [],
    }
    if meta["simulated"]:
        return out
    f = pd.read_csv(path / "samples.csv")
    if len(f) < 100:
        out["rows"] = len(f)
        return out
    f = f.reset_index(drop=True)  # file order is time order; sequence wraps
    t = f["monotonic_us"].to_numpy(np.int64)
    out["rows"] = len(f)
    out["duration_s"] = float((t[-1] - t[0]) / 1e6)
    dt = np.diff(t)
    out["median_period_ms"] = float(np.median(dt) / 1e3)
    heat = f["heater_active"].to_numpy(bool)
    edges = np.flatnonzero(np.diff(heat.astype(int)) == 1) + 1
    if heat[0]:
        edges = np.concatenate(([0], edges))
    for on in edges:
        off = on
        while off < len(heat) and heat[off]:
            off += 1
        lo, hi = contiguous_bounds(t, on)
        # don't let the window include another pulse
        other = edges[edges != on]
        prev_other = other[other < on]
        next_other = other[other > on]
        if len(prev_other):
            # end of that pulse + nothing; use its index as lower bound
            lo = max(lo, int(prev_other[-1]))
        if len(next_other):
            hi = min(hi, int(next_other[0]))
        t0 = t[on]
        pre_s = (t0 - t[lo]) / 1e6
        post_s = (t[hi - 1] - t0) / 1e6
        rel = (t - t0) / 1e6
        ch = {}
        for c in THERM:
            temp = f[f"{c}_temp_c"].to_numpy(float)
            valid = f[f"{c}_valid"].to_numpy(bool) & np.isfinite(temp)
            raw = f[f"{c}_raw"].to_numpy(float)
            base = (rel >= -min(20.0, pre_s)) & (rel <= -1.0) & valid
            base[:lo] = False
            alive = valid[lo:hi].mean() > 0.9 and np.std(raw[lo:hi]) > 0.3
            if base.sum() < 50 or not alive:
                ch[c] = {"alive": bool(alive)}
                continue
            p = np.polyfit(rel[base], temp[base], 1)
            rise = temp - np.polyval(p, rel)
            post = (rel > 2.0) & (rel < min(post_s, 180.0)) & valid
            post[:lo] = False
            post[hi:] = False
            peak = float(np.nanmax(rise[post])) if post.any() else float("nan")
            tpk = float(rel[post][np.nanargmax(rise[post])]) if post.any() else float("nan")
            ch[c] = {
                "alive": True,
                "noise_mK": 1e3 * robust_sigma(rise[base] ),
                "noise_counts": robust_sigma(np.diff(raw[base])) / np.sqrt(2),
                "peak_rise_C": peak,
                "t_peak_s": tpk,
                "baseline_C": float(np.polyval(p, -1.0)),
                "drift_mK_per_min": 60e3 * float(p[0]),
            }
        out["pulses"].append(
            {
                "run_id": int(f["run_id"].iloc[on]),
                "onset_index": int(on),
                "heater_s": float((t[off - 1] - t[on]) / 1e6 + np.median(dt) / 1e6),
                "pre_s": float(pre_s),
                "post_s": float(post_s),
                "v24_raw_on": float(f["v24_raw"].iloc[on:off].mean()),
                "excitation_raw": float(f["excitation_raw"].iloc[lo:hi].median()),
                "channels": ch,
            }
        )
    # idle stretches: contiguous blocks, trimmed 300 s away from any pulse
    near = np.zeros(len(t), bool)
    for on in edges:
        near |= np.abs(t - t[on]) < 300e6
    d = np.diff(t)
    bad = np.flatnonzero((d <= 0) | (d > MAX_GAP_US)) + 1
    bounds = np.concatenate(([0], bad, [len(t)]))
    for a, b in zip(bounds[:-1], bounds[1:]):
        seg = np.flatnonzero(~near[a:b]) + a
        if len(seg) == 0:
            continue
        # split on index gaps
        splits = np.flatnonzero(np.diff(seg) > 1) + 1
        for part in np.split(seg, splits):
            dur = (t[part[-1]] - t[part[0]]) / 1e6
            if dur >= 120:
                out["idle"].append(
                    {"start_index": int(part[0]), "end_index": int(part[-1]) + 1, "duration_s": float(dur)}
                )
    return out


def main() -> None:
    sessions = [survey_session(p) for p in sorted(CAPTURES.glob("session_*"))]
    (HERE / "survey.json").write_text(json.dumps(sessions, indent=1, default=float) + "\n")
    lines = [
        "# Capture survey",
        "",
        "| session | fw | rows | dur (s) | period (ms) | pulses | idle ≥120 s |",
        "|---|---|---:|---:|---:|---:|---|",
    ]
    for s in sessions:
        if s["simulated"]:
            lines.append(f"| {s['session']} | sim | | | | | |")
            continue
        idle = ", ".join(f"{i['duration_s']:.0f}" for i in s["idle"])
        lines.append(
            f"| {s['session']} | {s['firmware']} | {s.get('rows', 0)} | {s.get('duration_s', 0):.0f} | "
            f"{s.get('median_period_ms', float('nan')):.2f} | {len(s['pulses'])} | {idle} |"
        )
    lines += ["", "## Pulses", "",
              "| session | run | heater (s) | pre (s) | post (s) | v24 | alive | noise mK (median) | peak rise °C per channel |",
              "|---|---|---:|---:|---:|---:|---|---:|---|"]
    for s in sessions:
        for p in s["pulses"]:
            alive = [c for c, v in p["channels"].items() if v.get("alive") and "peak_rise_C" in v]
            noise = np.median([p["channels"][c]["noise_mK"] for c in alive]) if alive else float("nan")
            peaks = " ".join(f"{c}:{p['channels'][c]['peak_rise_C']:.2f}" for c in alive)
            lines.append(
                f"| {s['session']} | {p['run_id']} | {p['heater_s']:.2f} | {p['pre_s']:.0f} | {p['post_s']:.0f} | "
                f"{p['v24_raw_on']:.0f} | {''.join(c[1] for c in alive)} | {noise:.1f} | {peaks} |"
            )
    (HERE / "survey.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
