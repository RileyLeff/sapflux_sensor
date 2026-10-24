#!/usr/bin/env python3
"""Experiment 5: idle-time temperature summaries between pulses.

Between pulses the probes sit at stem temperature.  How many bytes/day does
it cost to report it, at what interval and resolution, and what is the
noise floor of each summary?  Uses the real idle blocks (Aug 24 quiet and
interference, Jul 11), converted ratiometrically to °C.

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/exp_idle.py
"""

from __future__ import annotations

import numpy as np
from scipy.ndimage import uniform_filter1d

import common as C
import entropy as K

OUT = C.HERE / "results"


def main() -> None:
    OUT.mkdir(exist_ok=True)
    out = {}
    for session, regime, raw, live in C.idle_blocks():
        exc = uniform_filter1d(raw[:, 0], 13, mode="nearest")
        temps = np.stack([C.raw_to_temp(raw[:, c], exc) for c in live], 1)
        res = {"seconds": len(raw) / C.FS}
        for interval in (60, 300, 1800):
            n = int(interval * C.FS)
            m = len(temps) // n
            if m < 3:
                continue
            means = temps[: m * n].reshape(m, n, -1).mean(1)
            # noise floor of one summary: sd of a short-window mean around a slow trend
            hp = temps - uniform_filter1d(temps, 25 * 120 + 1, axis=0, mode="nearest")
            nn = min(n, 25 * 30)
            mm = len(hp) // nn
            floor_mK = 1e3 * float(np.median(np.std(hp[: mm * nn].reshape(mm, nn, -1).mean(1), 0)))
            for step_mK in (1, 5, 10):
                qv = np.round(means * 1e3 / step_mK).astype(np.int64)
                bits = sum(K.rice_stream_bits(np.diff(qv[:, j])) for j in range(qv.shape[1]))
                per_sample_bits = bits / ((m - 1) * qv.shape[1])
                per_day = (86400 / interval) * 12 * per_sample_bits / 8 + 12 * 2  # + one absolute i16 anchor per channel
                res[f"{interval}s_{step_mK}mK"] = {"bits_per_value": per_sample_bits, "bytes_per_day_12ch": per_day}
            res[f"{interval}s_noise_floor_mK"] = floor_mK
            res[f"{interval}s_drift_sd_mK"] = float(1e3 * np.median(np.std(np.diff(means, axis=0), 0)))
        out[f"{regime}:{session}"] = res
        print(regime, session, {k: (round(v["bytes_per_day_12ch"]) if isinstance(v, dict) else round(v, 2)) for k, v in res.items()})
    C.save_json(out, OUT / "idle.json")


if __name__ == "__main__":
    main()
