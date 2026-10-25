"""Study A summary tables from results/study_a.json and study_a_crb.json.

Prints (and saves to results/study_a_summary.json) fitted/true speed,
direction error and RMSE per layout / offset / model / Kp / install
rotation, plus the CRB summary for the y next to the existing layouts.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pandas as pd

HERE = Path(__file__).resolve().parent
OUT = HERE / "results"
OFF_ORDER = ["0", "1.5a", "2a", "3a", "4a"]


def main():
    pd.set_option("display.width", 250)
    df = pd.read_json(OUT / "study_a.json")
    df["lay"] = df.layout + "_" + df.offset.astype(str)
    summ = {}
    st = df[(df.wound != "woodneedle") & (df.U == 10.0)]
    for wound in ("std", "prop"):
        g = st[(st.wound == wound) | (st.layout == "east30")]
        for val, nd in (("speed_ratio", 3), ("err_phi", 1), ("rmse_mK", 1)):
            p = g.pivot_table(index=["layout", "offset", "model"], columns=["beta", "Kp"], values=val)
            if val == "err_phi":
                p = p.dropna(how="all")
            print(f"== {val}, wound {wound}, 10 cm/h ==")
            print(p.round(nd).to_string())
            summ[f"{val}_{wound}"] = json.loads(p.to_json(orient="split"))
    wn = df[df.wound == "woodneedle"].pivot_table(index=["layout", "offset"], columns="model", values="speed_ratio")
    print("== wood-needle validation (no wounds, wood-property needles) ==")
    print(wn.round(3).to_string())
    lo = df[(df.U == 2.0)].pivot_table(index=["layout", "offset", "model"], columns="beta", values="speed_ratio")
    print("== 2 cm/h, Kp 300, std wound ==")
    print(lo.round(3).to_string())
    summ["woodneedle"] = json.loads(wn.to_json(orient="split"))
    summ["low_flow"] = json.loads(lo.to_json(orient="split"))

    crb = pd.read_json(OUT / "study_a_crb.json")
    keep = ["std_axial", "pair_0", "pair_2a", "pair_3a", "y3_1.5a", "y3_2a", "y3_3a", "y3_4a", "tri3", "Y3", "hex6", "dual4"]
    c = crb[crb.layout.isin(keep) & (np.isclose(crb.E_J_cm, 4.8))]
    for col in ("phi_med", "speed_med", "n_inf_phi"):
        if col in c.columns:
            p = c.pivot_table(index=["layout"], columns=["model", "U"], values=col)
            print(f"== CRB median over angle: {col} (E = 4.8 J/cm) ==")
            print(p.round(3).to_string())
            summ[f"crb_{col}"] = json.loads(p.to_json(orient="split"))
    (OUT / "study_a_summary.json").write_text(json.dumps(summ, indent=1))


if __name__ == "__main__":
    main()
