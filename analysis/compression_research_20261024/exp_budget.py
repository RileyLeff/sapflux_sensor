#!/usr/bin/env python3
"""Experiment 6: flash, cellular bytes and wake energy per tier.

Reads the measured per-pulse sizes from results/*.json and turns them into
days of flash, MB/month and a relative energy figure.  Every radio number
is a stated, editable assumption (ASSUME below), not a measurement.

    uv run --with numpy --with scipy --with pandas --with matplotlib python \
        analysis/compression_research_20261024/exp_budget.py
"""

from __future__ import annotations

import json
import math

import common as C

OUT = C.HERE / "results"

ASSUME = {
    "flash_bytes": 32 * 1024 * 1024,
    "flash_reserved_frac": 0.10,  # config, logs, wear slack
    "frame_header_B": 24,  # field-protocol record header (run id, onset time, flags, CRC) per pulse record
    "datagram_payload_max_B": 1100,
    "datagram_overhead_B": 28 + 29,  # IPv4+UDP 28, DTLS 1.2 record 29 (drop to 28 without DTLS)
    "ack_bytes_per_wake": 120,  # downlink ack + PSM/RAI signalling, counted as bytes
    "wake_energy_equiv_kB": 10.0,  # radio wake (RRC setup + tail with RAI) == energy of sending this many kB
    "wake_energy_J": 0.5,  # absolute anchor for the above (nRF9160 LTE-M, order of magnitude)
    "usd_per_MB": 0.50,
    "days_per_month": 30.44,
}


def main() -> None:
    enc = json.loads((OUT / "encoders.json").read_text())
    spec = json.loads((OUT / "encoder_spec.json").read_text())
    idle = json.loads((OUT / "idle.json").read_text())
    tiers = {
        "day-one 500 ms bins, i16": enc["dayone_500ms_i16"]["bytes_synthetic_mean"] - 32,
        "day-one 500 ms bins, Rice": enc["dayone_500ms_rice"]["bytes_synthetic_mean"] - 32,
        "log-time bins 4%": enc["logbins_4pct"]["bytes_synthetic_mean"] - 32,
        "SFK1 k12 (recommended)": spec["k12"]["payload_B_mean"],
        "SFK1 k16": spec["k16"]["payload_B_mean"],
        "lossless 25 Hz (mean8 Rice)": enc["lossless_mean8_rice"]["bytes_synthetic_mean"] - 32,
    }
    # idle summary: 5-min means, 10 mK, worst measured session
    idle_day = max(v["300s_10mK"]["bytes_per_day_12ch"] for v in idle.values() if "300s_10mK" in v)
    a = ASSUME
    usable = a["flash_bytes"] * (1 - a["flash_reserved_frac"])
    rows = []
    for name, b in tiers.items():
        rec = b + a["frame_header_B"]
        for ppd in (24, 48):
            row = {"tier": name, "payload_B_per_pulse": round(b, 1), "pulses_per_day": ppd,
                   "flash_days": usable / (rec * ppd)}
            for wakes in (ppd, 4, 1):
                day_payload = rec * ppd
                dgrams = wakes * math.ceil(day_payload / wakes / a["datagram_payload_max_B"])
                day_bytes = day_payload + dgrams * a["datagram_overhead_B"] + wakes * a["ack_bytes_per_wake"]
                energy_equiv_kB = wakes * a["wake_energy_equiv_kB"] + day_bytes / 1000
                row[f"wakes{wakes}"] = {
                    "MB_per_month": day_bytes * a["days_per_month"] / 1e6,
                    "usd_per_month": day_bytes * a["days_per_month"] / 1e6 * a["usd_per_MB"],
                    "radio_J_per_day": energy_equiv_kB / a["wake_energy_equiv_kB"] * a["wake_energy_J"],
                    "payload_share_of_radio_energy": (day_bytes / 1000) / energy_equiv_kB,
                }
            rows.append(row)
    out = {"assumptions": ASSUME, "idle_summary_B_per_day_5min_10mK": idle_day, "rows": rows}
    C.save_json(out, OUT / "budget.json")
    print(f"{'tier':32s} {'B/pulse':>8s} {'ppd':>4s} {'flash d':>8s} | MB/mo @wake/pulse, 4/day, 1/day | radio J/day @same | payload share @1/day")
    for r in rows:
        w = [r[f"wakes{k}"] for k in (r["pulses_per_day"], 4, 1)]
        print(f"{r['tier']:32s} {r['payload_B_per_pulse']:8.0f} {r['pulses_per_day']:4d} {r['flash_days']:8.0f} | "
              + " ".join(f"{x['MB_per_month']:6.2f}" for x in w) + " | "
              + " ".join(f"{x['radio_J_per_day']:6.2f}" for x in w) + f" | {w[2]['payload_share_of_radio_energy']:.0%}")
    print(f"idle summary (5-min means, 10 mK, 12 ch): {idle_day:.0f} B/day")


if __name__ == "__main__":
    main()
