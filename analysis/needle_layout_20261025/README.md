# Flex-PCB needles: a lowercase-y layout, and how straight the holes must be

Design study, 2026-10-25, for the all-in-one sensor with flex-PCB needles:

- 0.9 mm heater;
- 21G (0.82 mm) sensor needles;
- 4.8 J/cm heater energy;
- k ≈ 0.0025 cm²/s;
- 0.6 cm axial spacing.

Two questions:

- **A.** Does offsetting the sensors sideways ("lowercase y") get them out of the heater's stagnant strip, and does it beat a collinear pair?
- **B.** How far can the sensor holes of 12 cm needles drift from the heater before the velocity profile is wrong by more than max(0.5 cm/h, 5 %)? That number is the accuracy spec for a motorized drilling jig.

The physics is reused from `../flow_vector_research_20261024/` without changes:

- `needle_cost.py`: Darcy flow plus Crank–Nicolson advection–diffusion, with steel needles and wound annuli;
- `vecmodel.py`: the analytic line-source model with blur and loss, its fits, and the CRB;
- `../fit_4x4_pulse.py`: `fit_pair`.

`fd2d.py` generalizes `needle_cost` to per-needle radii and wounds, plus area-fraction (anti-aliased) masks. It reproduces `needle_cost.simulate` to 9e-16. The time loop also runs in Rust (`fdstep/`), which matches the Python loop to 1.4e-14 at a 1.1 °C peak (`fd2d.py check`).

Every number below comes from a script in this directory (see "Reproduce").

## 0. Answers in brief

**A. The lowercase y beats the collinear layouts, at an offset of 3a (2.4 mm), when fitted with flow along the grain and R = kL/kT pinned (`aligned_R`).** Here a = 0.08 cm, the heater's blocked half-width (needle plus wound).

Fitted/true speed, geometry fixed at truth, 10 cm/h, wound = needle + 0.35 mm per side, over Kp 1–3000 and installs at 0° and ±20°:

| layout and fit | vertical | rotated ±20° | all cases | Kp ≤ 300, all rotations |
|---|---|---|---|---|
| East30 1.3 mm collinear, `fit_pair` | 0.76–0.79 | 0.50–0.70 | 0.50–0.79 | 0.61–0.79 |
| collinear 0.9 mm / 21G pair, axis fit | 0.82–0.86 | 0.66–0.74 | 0.66–0.86 | 0.71–0.86 |
| offset pair 3a, axis fit | 0.78–0.87 | 0.56–1.03 | 0.56–1.03 | 0.58–1.03 |
| **lowercase y 3a, `aligned_R`** | 0.76–0.87 | 0.77–0.85 | **0.76–0.87** | **0.82–0.87** |
| lowercase y 3a, `general_R` | 0.77–0.87 | 0.84–0.98 | 0.77–0.98 | 0.83–0.98 |

What these numbers show:

- What matters for a field probe is not the mean speed loss, which a calibration factor removes, but how much it changes with Kp and install rotation.
  - The y at 3a with `aligned_R` has a spread of 0.10 (0.05 at Kp ≤ 300).
  - The collinear pair spreads 0.20 and East30 0.28–0.30.
  - Its direction error is ≤ 3.4°.
- **An offset pair is worse than collinear when the install is rotated.** It reads 0.56 one way and 1.03 the other, because the tangential flow projects onto the sideways offset.
  - The y's two downstream sensors at ±offset cancel that term.
- **Small offsets (1.5a–2a) do not help at Kp ≥ 30.** The heat source sits inside the heater's own stagnant strip, and each sensor's wound has its own strip. See §A.3.
- **The first-order expectations hold in isolation but are not what decides the result.**
  - The flow beside the heater does speed up by the flat-plate factor: ×1.19 at 2a and ×1.07 at 3a at x = 0, Kp 300, against theory ×1.155 and ×1.061.
  - The 60 s signal drops 4.2 % at 2a and 9.2 % at 3a with isotropic k, as expected. With kL/kT = 1.85 the sideways spread uses kT, so the drops are 7.6 % and 16.4 %.
- **CRB.** The y needs `aligned_R` or `general_R`. With R free (`general`) it is not identifiable: two pair orientations mod 180° are not enough (flow_vector README §2.4), so those fits return 5–270× the true speed.
  - At 10 cm/h, y3 3a `aligned_R` gives 4.4° and 14 % speed, against 2.5° and 7 % for tri3.
  - Its speed CRB in the axis model is 5.5 %, the same as the pair.

**B. With the user's position-from-trace method (`fit_pair`, the derivative/Marshall fit that solves the up/down spacing asymmetry from the data), the jig spec is 0.25° per hole, angle relative to the heater, with entry offset ≤ 0.2 mm.**

Conditions: day profile peaking at 15 cm/h at 1.75 cm depth, zero at the sapwood/heartwood boundary at 8 cm; thermistors every 3 mm to 11.85 cm; criterion max(0.5 cm/h, 5 %) at every depth.

What passes and fails:

- 0.25° per hole holds for the worst geometric case, both holes converging toward the heater. It fails at 0.5°, with a worst error of 1.11× the threshold.
- Other drift modes are looser:
  - one hole drifting toward the heater: 0.5° (1.19× at 1°);
  - away from the heater: 1°;
  - both holes leaning the same way (equivalent to the heater leaning): ≥ 2°;
  - sideways: 1° (both holes) to ≥ 2° (one hole).
- With FD truth including wounds, Kp 300 and one wound calibration factor, the same modes pass at 0.5° (both converging, 0.99×), 1°, 2° and 0.5–1°.
- In a Monte Carlo with random drift per hole (sd σ) and entry sd 0.1 mm, `fit_pair` passes at every depth in 100 % of draws at σ = 0.1°, 92–96 % at σ = 0.25–0.5°, and 46 % at σ = 1°.
- At low flow (peak 3 cm/h) everything is 4–8× looser, because the 0.5 cm/h floor dominates.

| drift mode, day profile 15 cm/h | naive HRM (0.6 cm, k fixed) | fixed-spacing whole-curve fit | **`fit_pair` (user's method)** | `fit_pair` + amplitude spacing (ρc, R known) | structured line fit (noise-free, analytic truth) |
|---|---|---|---|---|---|
| one hole, toward heater | < 0.25° | 0.25° | **0.5°** | ≥ 2° | ≥ 2° |
| both holes converging | 0.25° | 0.5°* | **0.25°** | ≥ 2° | ≥ 2° |
| both holes parallel (heater lean) | < 0.25° | 0.25° | **≥ 2°** | ≥ 2° | ≥ 2° |
| one hole sideways | 0.25° | 1° | **≥ 2°** | ≥ 2° | ≥ 2° (struct_xy) |
| per-depth θ\*(z) from the sweep: converging / one hole / parallel | 0.27° / 0.14° / 0.07° | 0.52° / 0.40° / 0.26° | **0.46° / 0.85° / 2.96°** | ∞ / ∞ / 2.97° | n/a |

\* passes 0.25° and 0.5°, fails 1° (0.98× at 0.5°).

**Why converging drift sets the spec.** Common-mode spacing error is exactly degenerate with V in any fit with mean spacing nominal: (x̄, k, V) → (f·x̄, f²·k, f·V). A pair whose holes each converge by 0.3 mm (5 % of the 6 mm spacing) reads 5 % high. Data cannot fix that unless the mean spacing comes from the amplitude, which needs ρc, heater energy and R: x̄ ∝ (ρc)^−½ R^¼.

- A 10 % ρc error alone is a 4.7 % velocity error, nearly the whole budget.
- So the amplitude route is not a way around the jig spec unless ρc is measured per tree.

**The structured joint line fit** (entry + slope per hole across all depths, heartwood as zero-flow anchors) **works perfectly on noise-free analytic data but fails with wounds.**

- On analytic data it recovers every drift case to 2°. Its CRB on the common-mode slope is 0.024° per pulse (1-pulse σV ≤ 0.03 cm/h), so noise is not the limit.
- With FD truth (wounds), the wound bias scales with V(z). The fit reads the depth-varying bias as hole convergence: a fake 0.53–0.65° per hole at nominal geometry.
  - It then fails even with no drift: 1.56× the threshold for struct_x and 1.41× for struct_xy.
- The structured fit is the right idea, but it needs a wound-aware forward model before it can relax the jig spec. Until then, design to the `fit_pair` numbers.

## 1. Conventions

- **Coordinates.** x is the axial direction (along the grain, flow +x, up the stem); y is tangential; z is depth below the bark surface. The heater is at the origin.
- **Speeds.**
  - Study A: fitted/true speed at 10 cm/h, plus 2 cm/h at Kp 300.
  - Study B: V(z) in cm/h.
- **Wounds.**
  - Standard: needle + 0.35 mm per side, i.e. radius r + 0.035 cm. That is 0.45 + 0.35 = 0.8 mm for the heater (= a) and 0.76 mm for 21G.
  - Proportional sensitivity case: radius × 2/1.3, the East30 wound-to-needle ratio.
  - East30 itself: 1.3 mm needle, 2 mm wound, as in flow_vector §6.
- **Kp** is the axial/tangential Darcy permeability ratio, swept 1, 30, 300, 3000 (flow_vector §6).
- **"Rotated β"**: the probe is rotated by β relative to the grain, so in the probe frame the flow is at −β.
  - Earlier work used +20° only.
  - Offset layouts are not mirror-symmetric, so ±20° are reported separately here.
- **Noise and fits.**
  - Noise where used: 5 mK per 25 Hz sample, one pulse.
  - All FD fits are noise-free, with geometry fixed at truth unless stated: positions and gains at sd 1e-4.

## A. Study A: the lowercase y offset layout (`study_a.py`)

### A.1 Layouts

| name | sensors (x, y) cm | notes |
|---|---|---|
| `east30` | (±0.6, 0) | 1.3 mm needles, 2 mm wound: the §6 baseline |
| `pair` offset y₀ | (+0.6, +y₀), (−0.6, −y₀) | point-symmetric classic pair; y₀ = 0 is the collinear small-needle pair |
| `y3` offset y₀ | (+0.6, +y₀), (+0.6, −y₀), (−0.6, −y₀) | **lowercase y** |

Offsets: y₀ = 0, 1.5a, 2a, 3a, 4a with a = 0.08 cm, i.e. 0, 1.2, 1.6, 2.4, 3.2 mm.

**Choosing the lowercase y.** I chose the cleanest 3-sensor version:

- two downstream sensors at ±y₀, so the downstream side is symmetric and the tangential term in the amplitude b = 0.6·uₓ/kL + y·u_y/kT cancels in their mean;
- one upstream sensor at −y₀, point-symmetric to one of them.

That gives pair directions in two orientations mod 180°: the two up/down pairs are mirror images, and the downstream pair is tangential. This is the minimum for an aligned fit with R known. The alternative, upstream at y = 0, puts that sensor in the heater's strip again.

**Fits** (vecmodel, blur + loss free):

- `axis`: flow along the probe axis, R = 1.85 known. This is all a 2-sensor pair supports.
- `aligned_R`: flow along a free grain angle, R pinned.
- `general_R`: flow and grain free, R pinned.
- `aligned` / `general`: R free.

Also reported: the naive HRM (60–80 s, the downstream sensor against its point-symmetric partner) and, for East30, `fit_pair`.

**FD.**

- dx = 0.01 cm (half of needle_cost's grid), so the 0.82 mm needles span 8 cells.
- Heat domain x ∈ [−2, 2.6], y ∈ [−1.5, 1.5]; Darcy domain needle_cost's x ∈ [−12, 13] cm, with y cut to ±1.5.
- 268 cases. The first 182 ran in the Python loop and the rest in the Rust loop; the two are identical (above).

**Validation.** With wood-property needles and no wounds, the R-pinned fits return 1.000 for every layout and offset, and HRM 0.997. With R free, the y returns 0.955–0.990, the first sign that R is not identified.

### A.2 Results (std wound, 10 cm/h, fitted/true speed; `fig_a_speed.png`)

| layout | fit | vertical Kp 1 / 30 / 300 / 3000 | +20° Kp 1 / 30 / 300 / 3000 | −20° Kp 1 / 30 / 300 / 3000 |
|---|---|---|---|---|
| east30 | fit_pair | 0.79 / 0.77 / 0.79 / 0.76 | 0.70 / 0.66 / 0.61 / 0.50 | same as +20° |
| east30 | HRM | 0.78 / 0.73 / 0.74 / 0.72 | 0.73 / 0.70 / 0.65 / 0.52 | same |
| pair 0 | axis | 0.86 / 0.83 / 0.83 / 0.82 | 0.74 / 0.73 / 0.71 / 0.66 | same |
| pair 1.5a | axis | 0.86 / 0.82 / 0.76 / 0.62 | 0.64 / 0.65 / 0.65 / 0.61 | 0.88 / 0.84 / 0.79 / 0.68 |
| pair 2a | axis | 0.87 / 0.83 / 0.78 / 0.65 | 0.62 / 0.63 / 0.63 / 0.59 | 0.93 / 0.89 / 0.87 / 0.79 |
| pair 3a | axis | 0.87 / 0.86 / 0.85 / 0.78 | 0.58 / 0.59 / 0.59 / 0.56 | 1.03 / 0.99 / 0.99 / 0.95 |
| pair 4a | axis | 0.88 / 0.88 / 0.88 / 0.82 | 0.54 / 0.56 / 0.56 / 0.53 | 1.14 / 1.08 / 1.03 / 0.88 |
| y3 1.5a | aligned_R | 0.85 / 0.79 / 0.72 / 0.59 | 0.82 / 0.80 / 0.78 / 0.69 | 0.84 / 0.80 / 0.75 / 0.62 |
| y3 2a | aligned_R | 0.86 / 0.80 / 0.74 / 0.61 | 0.83 / 0.82 / 0.81 / 0.74 | 0.85 / 0.82 / 0.80 / 0.71 |
| **y3 3a** | **aligned_R** | **0.87 / 0.84 / 0.83 / 0.76** | **0.83 / 0.82 / 0.82 / 0.77** | **0.85 / 0.82 / 0.82 / 0.78** |
| y3 4a | aligned_R | 0.87 / 0.86 / 0.86 / 0.80 | 0.85 / 0.84 / 0.82 / 0.74 | 0.87 / 0.83 / 0.79 / 0.67 |
| y3 3a | general_R | 0.87 / 0.84 / 0.83 / 0.77 | 0.89 / 0.89 / 0.89 / 0.84 | 0.98 / 0.96 / 0.96 / 0.91 |
| y3 3a | HRM | 0.85 / 0.83 / 0.82 / 0.76 | 0.68 / 0.70 / 0.70 / 0.66 | 0.92 / 0.87 / 0.87 / 0.82 |

**Direction error** for y3 `aligned_R`:

- vertical: ≤ 2.2°;
- rotated: 0.4–5.7° (3a: ≤ 3.4°, 4a: ≤ 3.1°, 1.5a: up to 5.7°).

`general_R` behaves differently:

- At +20° it is ≤ 3° (3a: ≤ 0.7°), except 4a at Kp 3000 (−20°).
- **At −20° it is 12–20° off at every offset.** This is not an optimizer failure. From 16 starts, the minimum with grain at 67° and kL 0.0034 fits the FD data better (cost 8140) than the near-truth solution (8431).
  - At −20° the flow in the probe frame is at +20°. The heater's wake reaches x = 0.6 cm at y = 0.6·tan 20° = 0.22 cm, which is where the (0.6, +3a = 0.24) sensor sits.
  - The homogeneous model has no way to represent a sensor in the heater's wake, so with the grain angle free it bends the grain to explain it.
  - `aligned_R` ties the flow to the grain and stays within 2°.
- The same mechanism gives `general_R` a spurious minimum at 4a vertical (0.65–0.67).

**Proportional wound** (radius × 2/1.3) raises all speeds by 0.03–0.05 and leaves the ranking unchanged. y3 3a `aligned_R` is then 0.82–0.90.

**At 2 cm/h** (Kp 300, std wound; vertical / +20° / −20°):

| layout and fit | vertical | +20° | −20° |
|---|---|---|---|
| East30 fit_pair | 0.79 | 0.61 | 0.61 |
| collinear pair (axis) | 0.85 | 0.72 | 0.72 |
| y3 3a `aligned_R` | 0.83 | 0.88 | 0.82 |

**Fit RMSE** (mK):

| layout and fit | RMSE |
|---|---|
| East30 | 4–6 |
| collinear pair | 2.5–4.6 |
| offset pairs | 0.7–6.5 |
| y3 `aligned_R` / `general_R` | 1–6 |
| y3 axis model, rotated (wrong model) | 7–20 |

So the systematic misfit is near or below one pulse's noise, and a fit's RMSE cannot be used to detect the wound bias.

### A.3 First-order checks (`results/study_a_firstorder.json`, `fig_a_firstorder.png`)

The check uses a single heater with its wound (a = 0.08 cm) and the FD Darcy field.

**Axial flux at x = 0, relative to the far field**, against the flat-plate theory y/√(y² − a²):

| offset | theory | Kp 30 | Kp 300 | Kp 3000 |
|---|---|---|---|---|
| 1.5a | 1.342 | 1.51 | 1.45 | 1.38 |
| 2a | 1.155 | 1.22 | 1.19 | 1.13 |
| 3a | 1.061 | 1.08 | 1.07 | 1.03 |
| 4a | 1.033 | 1.04 | 1.03 | 1.01 |

At the sensor (x = 0.6) and Kp 30 the speed-up has decayed, e.g. 0.98 at 1.5a. At Kp ≥ 300 it persists: 1.21 at 1.5a and 1.14 at 2a.

**No-flow signal at 60 s, offset sensor relative to an on-axis sensor:**

| offset | isotropic k | kL/kT = 1.85 |
|---|---|---|
| 1.5a | 0.976 | 0.956 |
| 2a | 0.958 | 0.924 |
| 3a | 0.908 | 0.836 |
| 4a | 0.842 | 0.727 |

These match exp(−y²/4kt).

**So why don't offset sensors read high?** The speed-up is real, but:

1. The heat source is inside the heater's own stagnant strip, which reaches a·√Kp = 1.4 cm up and downstream at Kp 300. Heat has to diffuse sideways out of that strip before it is advected at all, whichever sensor is reading it.
2. Each 21G sensor's own wound casts its own strip, so an offset sensor still sits in a stagnant zone, its own.
3. At 1.5a–2a the sensor is in the shear layer at the strip edge, where the fitted single "speed" is a mixture.

The net effect is that vertical offset pairs and ys read *lower* than collinear at 1.5a–2a for Kp ≥ 30. They recover to about 0.85–0.88 at 3a–4a for Kp ≤ 300. The ×1.06 speed-up at 3a is visible as the small gain over collinear (0.85 vs 0.83 at Kp 300).

### A.4 CRB next to the existing layouts (`results/study_a_crb.json`)

Bayesian CRB, `fisher_needles` conventions:

- 5 mK at 25 Hz, one pulse, E = 4.8 J/cm;
- positions sd 0.03 cm, gains 2 %;
- needle blur as built;
- median over 12 flow angles, 0–165°.

`_R` means R pinned at sd 1e-3. Each cell is direction sd / speed sd.

| layout | needles | general_R, 2 cm/h | general_R, 10 cm/h | aligned_R, 2 cm/h | aligned_R, 10 cm/h | axis, speed 2 / 10 cm/h |
|---|---|---|---|---|---|---|
| std_axial (East30 line) | 3 | ∞ | ∞ | ∞ (11/12) | ∞ (11/12) | 21.5 % / 5.5 % |
| pair 0 / 2a / 3a | 3 | ∞ | ∞ | ∞ | ∞ | 21.5–21.7 % / 5.5–6.2 % |
| y3 1.5a | 4 | 26.5°, 81 % | 10.9°, 28 % | 16.6°, 66 % | 7.1°, 27 % | 18.7 % / 5.1 % |
| y3 2a | 4 | 21.0°, 63 % | 8.5°, 23 % | 13.0°, 54 % | 5.7°, 20 % | 18.7 % / 5.3 % |
| **y3 3a** | 4 | 14.8°, 44 % | 6.1°, 15 % | **9.5°, 40 %** | **4.4°, 14 %** | 18.8 % / 5.5 % |
| y3 4a | 4 | 12.2°, 34 % | 5.1°, 12 % | 7.9°, 32 % | 3.6°, 11 % | 18.9 % / 5.7 % |
| tri3 | 4 | 7.9°, 25 % | 2.5°, 7 % | 3.3°, 25 % | 2.5°, 7 % | 24.9 % / 6.7 % |
| Y3 (flow_vector) | 4 | 17.3°, 56 % | 4.7°, 16 % | 3.8°, 35 % | 3.2°, 13 % | 43.5 % / 12.6 % |
| hex6 | 7 | 5.6°, 18 % | 1.8°, 4.6 % | 2.3°, 18 % | 1.7°, 4.6 % | 17.6 % / 4.3 % |
| dual4 | 4 | 8.7°, 28 % | 2.7°, 6.8 % | 2.4°, 24 % | 2.3°, 6.5 % | 29 % / 7.1 % |

How to read this:

- **The y measures speed along the axis as well as the pair does.** In the axis model it is 5.1–5.7 % at 10 cm/h, against 5.5 % for the pair. It also gives direction and grain angle, which a pair cannot. The direction is weaker than tri3 because two of its three pair directions are nearly parallel.
- **With R free**, the y has 2–4 of the 12 angles with an infinite direction bound. Do not fit it with R free.
- **At 11.19 J/cm** (the flow_vector energy) the bounds barely change. For hex6 at 10 cm/h, `general` is 1.81° against 2.05° at 4.8 J/cm, and `general_R` 1.63° against 1.76°. As flow_vector §0 found, calibration limits these bounds, not energy.

## B. Study B: drilling-drift tolerance (`study_b.py`, `depth_kernel.py`, `analyze_b.py`)

### B.1 Geometry, wood, and the slice approximation

**Probe.** 12 cm needles: a straight heater, downstream sensor at x = +0.6, upstream at −0.6, thermistors every 3 mm from z = 0.15 to 11.85 cm. Each sensor hole is a straight line relative to the heater:

- x(z) = x_nom + eₓ + tan θₓ · z
- y(z) = e_y + tan θ_y · z

At 10 cm depth, 1° is 1.75 mm.

**Wood (assumed).**

| zone | depth (cm) | kL (cm²/s) | ρc (J/cm³/K) |
|---|---|---|---|
| bark | 0–0.5 | 0.0018 | 2.0 |
| sapwood | 0.5–8 | 0.0026 → 0.0024 (linear, drier inward) | 2.8 |
| heartwood | 8–12 | 0.0021 | 2.3 |

R = 1.85 in every zone. V = 0 in bark and heartwood.

**Flow profile in sapwood**, with d = z − 0.5:

- V = Vpk·(1 − (1 − d/1.25)²) for d ≤ 1.25;
- V = Vpk·((7.5 − d)/6.25)^1.5 beyond.

This peaks at z = 1.75 cm, 1.25 cm into the sapwood, and is zero at the boundary at z = 8. Vpk = 15 cm/h is the day profile; 3 cm/h is low flow.

**Slices are exact for the geometry.** For a straight infinite line heater in wood whose properties do not vary with depth, the temperature at a point is the integral of the 3-D point kernel along the line. That equals the 2-D solution at the point's in-plane offset, for any tilt of the sensor hole. Numerically, `depth_kernel.py` gives a relative difference of 0.0. So a tilted hole is a set of 2-D slices with local spacing.

**What slices neglect is depth variation in V, k and ρc.** The heat at a thermistor at time t has wandered in depth by free diffusion. Its effective depth kernel is the mixture over τ ∈ [0, t] of N(0, 2·k_r·τ), with variance k_r·t:

| t | FWHM, k_r = kT | FWHM, k_r = kL |
|---|---|---|
| 20 s | 2.3 mm | 3.1 mm |
| 40 s | 3.3 mm | 4.5 mm |
| 60 s | 4.0 mm | 5.5 mm |
| 120 s | 5.7 mm | 7.7 mm |

This is about the 3–5 mm expected. 47 % of the 60 s kernel lies within ±1.5 mm, half a thermistor pitch.

**Bounding that error.** I blurred the day profile with the kernel. The worst error is 1.4 cm/h (40 s kernel) and 1.7 cm/h (60 s) at z = 0.45, the step from bark to sapwood.

- Below z = 0.75 cm it is ≤ 0.56 cm/h (40 s) and ≤ 0.79 cm/h (60 s), at the peak kink.
- So the profile's own sharpest features are blurred by about the criterion. That blur is common to every estimator and to the nominal geometry, so it does not change the drift tolerances.
- The kink in this assumed profile is sharper than real sapwood probably is.

**Heater end.** With an insulated bark surface and the deep heater end at 12 cm, the end factor is:

| z | t = 20 s | t = 60 s | t = 120 s |
|---|---|---|---|
| 11.55 cm | 0.97 | 0.87 | 0.79 |
| 11.85 cm | 0.74 | 0.65 | 0.60 |

It is 1.0 by 11.25 cm at t ≤ 20 s and 0.97 at 60 s. This affects only heartwood thermistors (V = 0). A ratio method with matched up/down needle lengths cancels it. A whole-curve fit sees it as a loss term.

### B.2 Estimators

| name | what it is |
|---|---|
| `hrm` | naive heat ratio, spacing 0.6 cm, k = 0.0025, 60–80 s (fit_pair's stock velocity) |
| `axis_nom` | vecmodel whole-curve fit, flow along x, R known, **positions fixed at nominal**: the naive fixed-spacing fit |
| **`fit_pair`** | **the project's method of finding probe position from the trace** (`../fit_4x4_pulse.py`; top-level README and `intro.md`). α(t) = ζ − ε·τ/t_eff fits the up/down spacing asymmetry ε from the data; mean spacing is nominal. |
| `fit_pair_amp` | fit_pair with the mean spacing from the amplitude, x̄ = R^¼·√(E/(4π·ρc·A)), with ρc and R known (best case) |
| `axis_free` | vecmodel fit with every needle displacement free under a 0.03 cm prior: the repo's free-geometry fit (flow_vector §3/§6) |
| `struct_x` | one joint fit over all depths: entry + slope of both holes in x, blur, per-depth kL and amplitude, per-depth V in sapwood and bark, **V = 0 fixed in heartwood** (anchors); entry prior sd 0.01 mm |
| `struct_xy` | struct_x plus y entry and slope of both holes |
| `struct_x_e01` / `_kzone` / `_noanchor` | entry prior sd 0.1 mm / kL per zone instead of per depth / no heartwood anchor |

`fit_pair` is the clear single position-from-data method in the repo. The free-geometry vecmodel fit (`axis_free`) is reported alongside.

### B.3 Per-depth displacement tolerance (`results/study_b_sweep_tol.json`)

These are analytic curves at sapwood properties. δ\* is the smallest displacement at which |V̂ − V| > max(0.5, 5 %), taking the worse sign, in mm:

| displacement | hrm | axis_nom | fit_pair | fit_pair_amp |
|---|---|---|---|---|
| differential (both holes shift the same way = heater offset) | 0.05–0.10 | 0.16–0.36 | 2.5 (V ≥ 10) – 3.4 (V = 2); > 3.5 at V ≤ 1 | 2.6–3.4 |
| one hole along the axis | 0.14–0.26 | 0.21–0.71 | 0.47 (V ≥ 10), 0.77 (V = 6), 2.8 (V = 1) | > 3.5 |
| common mode (converging) | 0.15 (V ≥ 10), 0.35 (V = 6) | 0.29 (V ≥ 10), 0.46 (V = 6) | 0.25 (V ≥ 10), 0.43 (V = 6) | > 3.5 |
| one hole sideways | 0.77–1.12 | 1.0–2.2 | 1.5 (V ≥ 10) – 3.4 (V = 2) | 2.4–2.9 |
| both holes sideways | > 3.5 | 1.0–2.6 | 1.1–2.6 | 1.6–3.0 |

**Per-depth angle tolerance.** θ\*(z) = atan(δ\*(V(z)) / z) gives the following minimum over sapwood at the day profile (`fig_b_tolerance.png`):

| mode | hrm | axis_nom | fit_pair | fit_pair_amp | depth of the minimum |
|---|---|---|---|---|---|
| common (converging) | 0.27° | 0.52° | **0.46°** | ∞ | 3.2 cm |
| one hole | 0.14° | 0.40° | **0.85°** | ∞ | 3.2 cm |
| differential | 0.07° | 0.26° | **2.96°** | 2.97° | 5.9 cm |
| one hole sideways | 0.81° | 1.59° | **2.35°** | 3.66° | 4.7 cm |
| both sideways | ∞ | 1.65° | **1.72°** | 2.54° | 4.7 cm |

The binding depth for converging drift is 3.2 cm. There the flow is still 10 cm/h, so the 5 % term applies, and the hole has drifted 3.2 cm.

- Deeper in the sapwood, V falls and the 0.5 cm/h floor takes over.
- In heartwood (V = 0) fit_pair is unaffected by any axial drift, because ε absorbs the asymmetry and ζ stays 0.
- The naive HRM fails there at 0.05–0.09°, reporting false heartwood flow from any up/down asymmetry.

At low flow (peak 3 cm/h), fit_pair's θ\* is 2.1° (converging), 3.2° (one hole) and 6° (differential).

### B.4 Whole-profile test with explicit drift (`results/study_b_summary.json`, `fig_b_profiles.png`)

Every depth from 0.15 to 11.85 cm must pass. Each cell is the largest tested angle (0.25, 0.5, 1, 2°) up to which all depths pass, with the worst error/threshold at 1° in parentheses.

**Analytic truth, day profile:**

| drift | hrm | axis_nom | **fit_pair** | fit_pair_amp | struct_x | struct_xy |
|---|---|---|---|---|---|---|
| downstream hole toward heater | < 0.25 (10.6) | 0.25 (2.7) | **0.5 (1.19)** | 2 (0.25) | 2 (0.00) | 2 (0.00) |
| downstream hole away | < 0.25 (15.0) | 0.25 (2.4) | **1 (0.85)** | 2 | 2 | 2 |
| upstream hole toward | < 0.25 | 0.5 | **0.5 (1.04)** | 2 | 2 | 2 |
| both converging | 0.25 (2.1) | 0.5 (2.1) | **0.25 (2.21; 1.11 at 0.5°)** | 2 (0.13) | 2 | 2 |
| both diverging | 0.5 | 0.5 | **0.5 (1.61)** | 2 | 2 | 1 |
| both parallel (= heater lean) | < 0.25 (25.7) | 0.25 (5.4) | **2 (0.20)** | 2 | 2 | 2 |
| downstream sideways | 0.25 (4.1) | 1 | **2 (0.15)** | 2 | 0.25 (2.7) | 2 |
| both sideways, same or opposite | 2 | 1 | **1 (0.31; 1.32 at 2°)** | 2 | 1 | 2 |

**Entry offsets** (worst error/threshold, day profile):

| entry offset | fit_pair | hrm | struct_xy |
|---|---|---|---|
| downstream toward 0.2 mm | 0.49 | 1.42 | 0.69 |
| downstream toward 0.5 mm | 1.07 | 3.47 | 1.51 |
| both toward 0.2 mm | 0.81 | 1.35 | 0.54 |
| downstream sideways 1 mm | 0.39 | 1.47 | 0.01 |

**Low-flow profile (peak 3 cm/h):** fit_pair, fit_pair_amp, struct_x and struct_xy pass every case to 2°; struct_x_kzone passes one-hole sideways drift only to 1°. HRM fails single-hole axial drift at < 0.25°, and axis_nom passes 0.25–2°.

**FD truth** (Kp 300, std wounds, 0.9 mm heater and 21G sensors, anti-aliased needle masks, dx = 0.02, every other thermistor = 20 depths; day profile).

First, each estimator gets one wound-calibration factor, the slope of V̂ on V at the nominal geometry over sapwood:

| estimator | hrm | axis_nom | fit_pair | fit_pair_amp | axis_free | struct_x | struct_xy |
|---|---|---|---|---|---|---|---|
| calibration factor | 0.81 | 0.84 | 0.87 | 0.88 | 1.01 | 0.79 | 0.86 |

After calibration, at the nominal geometry:

- fit_pair and fit_pair_amp: worst 0.05×;
- hrm: 0.25×;
- axis_nom: 0.30×;
- axis_free: 3.6×, because the free geometry absorbs the wound misfit (as in flow_vector §6);
- struct_x 1.56× and struct_xy 1.41× (§0).

Then:

| drift | hrm | axis_nom | **fit_pair** | fit_pair_amp |
|---|---|---|---|---|
| downstream hole toward | < 0.25 (13.2) | 0.25 (2.8) | **1 (0.94)** | 2 (0.09) |
| downstream hole away | < 0.25 | 0.25 | **1 (0.83)** | 2 |
| both converging | 0.5 | 0.25 | **0.5 (0.99 at 0.5°; 2.06 at 1°)** | 2 (0.06) |
| both parallel | < 0.25 | < 0.25 | **2 (0.10)** | 2 |
| downstream sideways | 0.25 | 1 | **1 (0.78)** | 1 |
| both sideways, same | 1 | 1 | **0.5 (1.27 at 1°)** | 0.5 |

Entry offsets under FD truth:

- downstream toward 0.2 mm: 0.36×; 0.5 mm: 0.86×;
- both toward 0.2 mm: 0.70×;
- downstream sideways 0.5 mm: 0.60×; 1 mm: 1.33×.

Wounds make sideways drift matter more, because a sensor moving sideways crosses its own and the heater's strip geometry. Axial tolerances are similar to or looser than the analytic case.

### B.5 Structured fit: what it can do and why it fails in wood

**Noise.** The CRB at nominal geometry, analytic truth, one pulse, 5 mK at 25 Hz (`results/study_b_struct_crb.json`):

| variant | common-mode slope sd | differential slope sd | V sd |
|---|---|---|---|
| struct_x | 0.024° | 0.0001° | ≤ 0.027 cm/h (median 0.017) |
| struct_xy | 0.024° | — | ≤ 0.033 cm/h |
| struct_x_kzone | 0.0003° | — | — |

Notes on the variants:

- struct_xy's y-slope sd is 0.018 rad. The sideways position enters only through y², so its sign is not identifiable and its size is weak near zero.
- The first-order degeneracy between converging drift and a linear depth gradient in k costs a factor of 80 (0.0003° → 0.024°), but even 0.024° is ten times inside the spec.
- The heartwood anchor does not change the slope bound at the nominal geometry. struct_x_noanchor is identical, because ε at V = 0 already pins the differential.

**Monte Carlo** (analytic truth, random drift: slopes N(0, σ) per hole in x and y, entries N(0, 0.1 mm); 24 draws per cell; `results/study_b_mc.json`). Each cell is the fraction of installs passing at every depth (90th-percentile worst error/threshold):

| σ per hole | hrm | fit_pair | fit_pair_amp | struct_x | struct_xy |
|---|---|---|---|---|---|
| 0.1° | 0.17 (4.1) | **1.00 (0.53)** | 1.00 (0.13) | 1.00 (0.56) | 1.00 (0.50) |
| 0.25° | 0.00 (9.4) | **0.92 (0.59)** | 1.00 (0.16) | 0.83 (1.72) | 1.00 (0.53) |
| 0.5° | 0.04 (16.7) | **0.96 (0.90)** | 1.00 (0.19) | 0.71 (2.43) | 0.96 (0.65) |
| 1° | 0.04 (30.5) | **0.46 (2.09)** | 1.00 (0.36) | 0.42 (4.86) | 1.00 (0.63) |

At 3 cm/h, fit_pair passes 100 % at every σ up to 1°.

- struct_x fails under random drift because it ignores the sideways component.
- struct_xy handles everything, *in the analytic model*.

**In wood with wounds** (FD truth) the structured fits are not usable yet. The wound bias depends on V, and V varies with depth, so it looks like a depth-varying spacing error, i.e. a hole slope.

- At nominal geometry, struct_x fits the downstream hole converging at 0.65° and the upstream hole at 0.53°, then reads V 10–22 % low with a non-uniform shape (RMSE 6 mK). Calibration cannot remove that.
- fit_pair reads each depth independently, so the same bias is a near-constant factor (0.87) that one calibration removes.

To use the structure, the forward model has to carry the wound, e.g. an FD-derived correction as a function of V, wound and Kp, or a fitted wound width (flow_vector §6, "Wound width").

### B.6 The spec

For the motorized jig, using `fit_pair` with one wound calibration factor:

| quantity | spec | binding case |
|---|---|---|
| angle of each sensor hole relative to the heater, any direction | **≤ 0.25°** | both holes converging, analytic: 0.59× at 0.25°, 1.11× at 0.5° |
| axial entry offset per hole | **≤ 0.2 mm** | 0.49× (one hole) and 0.81× (both toward) |
| sideways entry offset | ≤ 0.5 mm | — |
| as random errors per hole | **σθ ≤ 0.25–0.5°, σe ≤ 0.1 mm** | ≥ 92 % of installs pass at every depth (MC) |

**What can be relaxed:**

- If the heater itself leans, both holes "parallel", fit_pair absorbs it to ≥ 2°. The jig must hold the *relative* angle between holes, not absolute verticality to the stem.
- At low flow (3 cm/h peak) the spec relaxes to about 2°.

**For comparison, the naive estimators:**

- Fixed-nominal-spacing HRM needs θ\* = 0.07° (differential) to 0.14° (one hole) and fails every single-hole case at 0.25°. It reports false heartwood flow from any up/down asymmetry.
- The fixed-spacing whole-curve fit (`axis_nom`) needs about 0.25°. Under FD truth it fails even at 0.25° for a parallel lean (1.04×).

## Limitations

- **Study A.**
  - Fits are noise-free with geometry fixed at truth, so they show bias, not scatter. The CRB gives the scatter.
  - R free is not identifiable for the y; all y conclusions assume R known to about 0.1 %. With R known to a few percent, `aligned_R`'s direction degrades towards the CRB values above.
  - The Kp 3000 Darcy domain (x ∈ [−12, 13], y ±1.5) is short in stretched coordinates (|X| ≤ 0.24 cm), so Kp 3000 is only indicative. The same caveat applies to flow_vector §6.
  - The wake-shadow minimum of `general_R` at −20° is a model-error minimum, not an optimizer artefact. Other spurious minima may exist at rotations not tested.
  - Wound conductivity and FD needle properties are the needle_cost assumptions.
- **Study B.**
  - Depth slices are independent. The flow-profile blur (≤ 0.8 cm/h at the profile's kinks, 1.7 cm/h at the bark step) and the heater-end factor (deepest 0.6 cm only) are bounded in §B.1 but not simulated.
  - k, ρc and the V(z) profile are assumed, not measured. Drift tolerance scales with V(z) at the drift depth, so a profile with high flow deeper in the stem tightens the spec.
  - FD truth: dx = 0.02 cm with anti-aliased masks, 20 of the 40 depths, Kp 300 only, std wounds only.
  - The heater is the reference line and the jig is assumed to drill the heater straight. All angles are relative.
  - Fits are noise-free, apart from the structured-fit CRB. One pulse of noise adds about 0.03 cm/h for the structured fit, and more for per-depth fits.
  - fit_pair_amp assumes ρc, heater energy and R are known exactly. Its tolerance is an upper bound on what amplitude spacing could buy.
  - The MC uses 24 draws per cell, so pass fractions are ± about 0.06.
  - Positions in the y direction enter only at second order. A sideways drift's sign is not recoverable from heat data.

## Reproduce

Python runs from this directory under low priority, with at most 8 workers (WORKERS in each script). The Rust FD loop uses all cores:

    (cd fdstep && cargo build --release)
    nice -n 15 uv run --with numpy --with scipy --with pandas --with matplotlib python <script> [stage]

| script | output |
|---|---|
| `fd2d.py check` | needle_cost regression; Rust vs Python time loop |
| `study_a.py` | `results/study_a.json`, `study_a_firstorder.json`, `study_a_crb.json`, `fig_a_speed.png`, `fig_a_firstorder.png`. FD curves are cached in `results/study_a_curves.npz`, and only missing cases are simulated (Rust). About 10 min plus the FD. `study_a.py crb` and `study_a.py fig` run parts. |
| `analyze_a.py` | Study A tables, `results/study_a_summary.json` |
| `depth_kernel.py` | `results/depth_kernel.json`, `fig_b_kernel.png` |
| `study_b.py sweep` / `analytic` / `fd` / `crb` / `mc` | `results/study_b_*.json` (fd: 600 Rust FD cases, about 10 min) |
| `analyze_b.py` | Study B tables, `results/study_b_summary.json`, `fig_b_profiles.png`, `fig_b_tolerance.png` |

`results/*.npz` (curve caches), `results/study_a_crb_all.json`, `results/*_stdout.txt` (run logs) and `fdstep/target/` are ignored by git.
