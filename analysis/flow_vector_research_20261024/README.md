# Flow-vector heat pulse: can more needles or more heaters recover direction?

Design exploration, started 2026-10-24. The questions:

- Can a whole-curve fit (an extension of `../fit_4x4_pulse.py`) recover the sap flow vector?
- What needle layout should we build?
- What does each needle cost in wounding and blockage?

Every number below comes from a script in this directory (see "Reproduce" at the end). Noise throughout is 5 mK per 25 Hz sample, the board's measured noise (`real_capture.py`: 4.7–4.9 mK), with one pulse per heater unless stated.

## 0. Answers in brief

**Direction is identifiable in the axial–tangential plane, but not with the probe as it is now.** It takes pair directions in at least 3 distinct orientations (mod 180°). The East30 axial line (std_axial) has one orientation, so its direction is not identifiable. Axial + side layouts and the penta-needle cross have two orientations. They fail in the general model at the probe's own orientation (the CRB is infinite there), and the Monte Carlo puts their fitted angles 15–40° out at 2 cm/h.

**3-D (radial) flow is not measurable in practice** (sec. 2.6).

**What actually limits accuracy is calibration, not noise or needle count.** This is the main result of the math (sec. 2.2). For an ideal line source the flow enters each heater→sensor curve in only two ways:

- as an amplitude factor exp(b/2), which is exactly degenerate with thermistor gain × heater amplitude;
- as a decay term, which is exactly degenerate with any common-mode heat loss.

Real data contain that loss: the soaked-block capture shows a loss equivalent to about 10 cm/h of fake flow. So in single-heater layouts the flow comes entirely from amplitude ratios. Its error then scales linearly with the thermistor-gain error and inversely with speed.

| Single-heater, 2 % gain prior | angle sd at 2 cm/h | speed sd at 2 cm/h | angle sd at 10 cm/h |
|---|---|---|---|
| tri3 (4 needles) | 8.8° | 32 % | 4.8° |
| hex6 (7 needles) | 5.8° | 19 % | 2.3° |

At a 10 % gain prior, tri3 is 38° and hex6 is 27° at 2 cm/h.

**Dual-role needles remove the gain dependence.** Each needle carries a heater and a thermistor, and the heaters fire in turn. With heater energies whose ratios are known (electrically measured and bench calibrated), the flow is identified without trusting any thermistor gain:

| Dual-role layout, heater ratio 0.2 % | 2 cm/h | 10 cm/h |
|---|---|---|
| dual3 (3 needles) | 4.2°, 7.8 % | 4.1°, 7.1 % |
| dual4 (4 needles) | 4.1°, 7.5 % | 4.0°, 7.1 % |

These hold even with gains completely free (`fisher_gain.py`, no-blur model). The remaining floor is needle position (0.03 cm), which improves roughly as 1/spacing. The dual layouts reach this with the same 3 holes East30 uses today, or one more.

**Every needle biases speed low, and shadows make that worse.** With fixed (true) geometry (`needle_cost.py`):

| Case | fitted / true speed |
|---|---|
| Bare 1.3 mm steel needle, no wound | 0.87–0.94 |
| 2 mm wound | 0.80–0.88 |
| 3 mm wound | 0.54–0.76 |

- Sap flow is anisotropic Darcy flow, so each impermeable needle and its wound casts an axial **stagnant strip** of length about a·√Kp. Kp is the axial/tangential permeability ratio, swept from 1 to 3000.
- With the probe vertical, shadows cost only a few percent more.
- With the probe rotated 20°, shadows cross the heat paths. At Kp = 3000 the speed ratio falls to:
  - 0.41 for hex6, 0.43 for std_axial and 0.58 for tri3;
  - 0.67 for 2H_tri and 0.70–0.71 for dual3/dual4.
- **More needles mean more shadows.** The 7-needle hex6 has the best CRB and the worst shadow bias.

Direction is more robust than speed, with these biases at a 20° rotation and Kp up to 3000:

| Layout | direction bias |
|---|---|
| hex6, 2H_tri | ≤ 0.6° |
| dual4 | ≤ 1.7° |
| dual3 | ≤ 1.9° (6.5° with 3 mm wounds) |
| tri3 | 2.4–4.7° (7.6° with 3 mm wounds) |
| Y3 | −4 to −11° |

**The wounds also break the analytic model.**

- They leave 1–8 mK of systematic misfit, and 11–28 mK for dual layouts when rotated. Noise is 1.4 mK per point.
- If needle positions and gains are left free under their priors, the misfit leaks into them and the flow estimate becomes unreliable, off by a factor of several in the dual layouts.
- A wound/needle-aware forward model is a prerequisite for field use, not a refinement.

**Wound width itself is formally identifiable but practically not.**

- Changing the wound by 1 mm moves the curves by 13–157 mK.
- However, 99.5–99.99 % of that signature is absorbed by the analytic model's k, blur, amplitude and speed. It shows up as a speed bias, not as a recognisable wound signal.
- What remains (about 3 mK per mm) gives a formal sd of 0.002–0.011 cm, but only if the wound model in the fit is exactly right.

**Recommendation** (details in sec. 7):

- Layout: 4 dual-role needles in the "Y" geometry (dual4), with pair directions at 30°, 90° and 150° to the axis. Fall back to dual3 if a fourth hole is unacceptable.
- Spacing: at least 0.6 cm, preferably 0.8–1.0 cm.
- Heater energy: measured on every pulse.
- Heater amplitude and thermistor gain ratios: calibrated on the bench in agar.
- Install vertical. The layout is isotropic for identifiability, but alignment still matters because shadows then stay off the heat paths and the general model's bounds are best near the aligned case.
- Fit with the general model (flow free to cross the grain), not the aligned one. Flow 20° off grain fitted with the aligned model biases direction by 4–12°.
- Next modelling step: a wound/needle-aware forward model.

## 1. Conventions

- **Coordinates.** The plane normal to the needles: x is the installer's "axial" direction and y is tangential. Needles run radially along z.
- **Units.** cm, s, °C. Speeds are heat velocity in cm/h; converting to sap flux needs wood ρc and water content, which is outside this study.
- **Baseline properties.** kL = 0.0025 cm²/s along grain and kT = kL/1.85 across it (the anisotropy ratio R = 1.85 is an assumption, discussed in sec. 10).
- **Geometry.** Spacing s = 0.6 cm, 1 s heat pulse, peak rise 1.3 °C at 0.6 cm with no flow.
- **Layouts** are defined in `vecmodel.layouts()` and `fisher_needles.ring_layouts()`:
  - std_axial is East30 today;
  - Y3 is one heater plus thermistors at −30°, +30° and 90°;
  - dual3 and dual4 are triangle and Y geometries in which every needle heats and senses;
  - ringN is one heater plus N thermistors on a circle.

## 2. Forward model and identifiability

### 2.1 Model

Treat wood locally as homogeneous and anisotropic. Heat diffusivity is a tensor K, with principal values kL and kT and grain angle θ. Heat moves with velocity u. An infinitely long line heater emits energy Q per unit length over [0, d]. The temperature rise at offset r is

    T(r,t) = A/√det K · (1/d)∫₀^{min(t,d)} (1/τ) exp(−(r−uτ)ᵀK⁻¹(r−uτ)/(4τ)) ds,   τ = t − s,   A = Q/(4πρc).

`check_model.py` verifies two limits:

- the 1-D isotropic limit reproduces `fit_4x4_pulse.finite_pulse_response` to 1e-15 relative;
- the anisotropic kernel matches brute-force quadrature of a rotated Gaussian.

Each pulse window h, read on thermistor j, is modelled as

    y_hj(t) = b0 + b1·t + g_j · A_h · S(p_j − p_h, t),

where g_j is the channel gain, A_h the heater amplitude and p the needle positions (nominal plus unknown displacement). All pairs are fitted jointly.

### 2.2 Where the flow information lives

Expand the exponent:

    (r−uτ)ᵀK⁻¹(r−uτ)/(4τ) = a/(4τ) − b/2 + cτ/4,
    a = rᵀK⁻¹r,   b = rᵀK⁻¹u,   c = uᵀK⁻¹u.

So for every pair,

    S = exp(b/2) · F(a, c, t).

The factor exp(b/2) comes out of the source-time integral, so this holds for a finite pulse too. Three consequences follow.

1. **a carries geometry and K only, with no flow.** Peak timing shifts with flow only through c.
2. **b, the flow asymmetry, is a pure amplitude.** It is exactly confounded with g_j·A_h. The heat-ratio method, ln(T_down/T_up) = xV/k, is this term. That is why it needs symmetric spacing and matched sensors, and why the field applies "zero-flow offsets".
   - Moreover b_hj = φ(p_j) − φ(p_h) with φ(p) = pᵀK⁻¹u/2. This is a potential, so free per-heater amplitudes and free per-sensor gains absorb it completely, even with reciprocal pairs.
   - With one heater, A is free, so only the spread of thermistor positions about their centroid carries b information. The heater's position drops out. This is why Y3, whose thermistors cluster on one side, is worse than tri3 (4.7° vs 2.8° at 10 cm/h in the general model).
3. **c, the |u|² decay, is exactly confounded with a common-mode loss exp(−λτ).** This term is what makes the peak time depend on speed (the T-max idea). Section 5 shows real data need λ.

The two ways out are therefore:

- **known gains**, which limits single-heater layouts to calibration accuracy;
- **known heater amplitude ratios plus at least 3 non-collinear heaters.** In log T_hj = α_h + γ_j + φ(p_j) − φ(p_h), fixing α_h's differences pins φ(p_h) − φ(p_k) = (p_h − p_k)ᵀK⁻¹u/2 at no gain cost. Heater energy is measured electrically, and thermal contact only delays it; it does not lose it.

`fisher_gain.py` confirms both numerically. With gains free:

| Layout | flow angle sd |
|---|---|
| dual3, heater ratio 0.2 % | 4.2° |
| 2H_tri (2 heaters, one baseline direction) | ~3000°, unidentified |
| tri3 | ~3000°, unidentified |

A finite needle radius (the blur term, sec. 2.5) makes the cross term time-dependent and weakly breaks the factorisation. That gives dual layouts some gain-free information even without a heater prior (9° at 2 cm/h). It is a second-order effect, and I would not rely on it.

### 2.3 What today's 1-D fit reports when the probe is rotated

Put the thermistors at ±x on the probe axis, with grain and flow at angle θ from that axis and R = kL/kT. The isotropic 1-D model then reads a = x²/k_app and b difference = 2xV/k_app, giving

    V_app/U = cos θ / (cos²θ + R sin²θ),    k_app = kL / (cos²θ + R sin²θ).

`misalign_1d.py` runs the project's `fit_pair` on noise-free anisotropic curves and agrees with this closed form to within 1–3 %:

| θ | V_app/U at R = 1 | R = 1.85 | R = 2.5 |
|---|---|---|---|
| 10° | 0.987 | 0.964 | 0.947 |
| 20° | 0.948 | 0.867 | 0.814 |
| 30° | 0.882 | 0.735 | 0.652 |

Anisotropy roughly multiplies the misalignment error by R. The 1-D fit's residual at a 20° rotation is only 6.9 mK, about the size of model errors already present in real data. So the misalignment cannot be detected from the 1-D residual.

### 2.4 Layout conditions

- Full K needs pair directions in at least 3 orientations (mod 180°).
- w = K⁻¹u needs at least 3 non-collinear thermistor positions (single heater), or at least 3 non-collinear heaters with known ratios (gain-free).
- Cross and T layouts have only 0° and 90° directions. They cannot separate a tilt of the K axes from a tilt of the flow at the symmetric orientations. They work only if flow is assumed to follow the grain (the aligned model), which is exactly the assumption we want to test.

### 2.5 Finite needles and loss

- A heater of radius a_h and a sensor needle of radius a_t that averages over its cross-section act, to second order, like Gaussian blurs. The covariance becomes 2(Kτ + cI), with c = (a_h² + a_t²)/8 = 0.00106 cm² for 1.3 mm needles.
- The real capture fits c = 0.00146, and FD validation recovers 0.0011.
- Without c, the FD validation misfit is 50 mK; with it, 6.5 mK. This is not negligible: it is about 2.5 % of the peak at early times.
- The loss λ stands in for heat leaving the plane (finite heater length, conduction along the needle, boundaries).

### 2.6 Radial (needle-parallel) flow

For an infinite heater, u_z drops out exactly. For a heater of finite length, u_z enters only through the erf end factors. It is therefore seen only by thermistors within one diffusion length (about 1 cm) of a heater end, and there it is confounded with the end positions and with the bark boundary (`radial_flow.py`):

- The formal CRB on u_z is 0.008–0.02 cm/h.
- A u_z of 1 cm/h changes the near-end channels by about 22 mK.
- But a bark boundary with 25 % reflection and true u_z = 0 fits as u_z = −1.02 cm/h, u_x = 8.73 instead of 10, with only 1.2 mK residual.

Radial flow is identifiable only through the part of the model that is least trustworthy, so it should not be measured this way. The same end effects bias axial speed in thermistors near the bark.

## 3. Information analysis (Bayesian CRB)

Setup:

- Parameters: lkL, lkT, θ, u (2), blur c, loss λ_h, amplitude A_h, gain g_j, needle displacements, and per-pair baselines.
- Priors: positions 0.03 cm per coordinate, gains 2 %. 60 s of pre-pulse data, 180 s post.
- Jacobi-scaled eigen-inversion; directions in the null space are reported as infinite.

Results (`fisher_needles.py`, median over flow angle, general model):

| layout (needles) | 10 cm/h angle sd / speed sd | 2 cm/h angle sd / speed sd |
|---|---|---|
| std_axial (3) | not identifiable | not identifiable |
| T3 axial+side (4) | 11.6° / 38 %, infinite at 0° and 180° | 15.5° / 54 % |
| cross4 (5) | infinite at 0° and 90° | — |
| tri3 (4) | 2.8° / 9.8 % | 7.9° / 26 % |
| Y3 (4) | 4.7° / 16 % | 17.3° / 56 % |
| 2H_tri (5) | 3.1° / 7.7 % | 11.0° / 35 % |
| pent5 (6) | 1.9° / 5.5 % | 6.1° / 19 % |
| hex6 (7) | 1.8° / 5.6 % | 5.6° / 18 % |
| ring8 (9) | 1.5° / 4.1 % | 4.8° / 15 % |
| dual3 (3) | 3.3° / 7.2 % | 7.2° / 23 % |
| dual4 (4) | 2.3° / 5.3 % | 5.2° / 18 % |

Notes on this table:

- It includes the blur term and no heater-ratio prior.
- Adding a heater-ratio prior and dropping blur gives the gain-free numbers in sec. 0.
- In the earlier run without loss (`fisher.py`), hex6 is 2.1–2.5° at 10 cm/h. That run's "perfect geometry" bounds (0.02–0.3°) show that noise is 10–100× below the calibration limit. Spending more pulse energy does not help.

Diminishing returns:

- The per-needle information (`fig_needles_info.png`) is roughly flat from 4 to 9 needles in ring layouts. Going from 4 needles to 7 gains about 1.5× in angle.
- dual4 matches hex6 with 3 fewer holes.
- Spacing is a stronger lever because the errors are absolute position and gain errors. For hex6 aligned at 10 cm/h (`fisher.py` sweep): 3.1° at 0.4 cm, 2.1° at 0.6, 1.6° at 0.8, 1.25° at 1.0 cm.
- Doubling the position sd to 0.06 cm roughly doubles the errors.

## 4. Do real fits reach the bounds? (Monte Carlo, `mc_fits.py`)

Setup: 40 trials per cell; random positions, gains, drift and noise; the project's detrending; MAP multistart fit. This run used the model without blur or loss.

**At 10 cm/h, fits reach the CRB.**

| Layout, model | angle RMSE | CRB |
|---|---|---|
| hex6 general | 1.9–2.9° | 2.2–3.1° |
| tri3 general | 4.3–7.7° | 4.5–7.2° |
| 2H_tri general | 2.9–4.1° | 2.8–3.3° |

Speed RMSE is 3–8 %.

**At 2 cm/h, multistart failures appear** (5–12 % of trials more than 20° off for tri3 and 2H_tri). T3 and cross4 in the general model are 15–42° RMSE with 20–40 % of trials more than 20° off.

Confounders, at φ = 25°:

- **Isotropic-K fit in R = 1.85 wood** (what penta-needle soil probes assume): direction stays fine (hex6 2.0°), but speed is biased by −12 to −19 %.
- **Flow 20° off grain, fitted with the aligned model:** direction bias of 4–12° at 10 cm/h. The general model is unbiased (hex6 2.6° RMSE).
- **Unmodelled baseline curvature (±2 mK/min²):** harmless at 10 cm/h (hex6 1.9°). At 2 cm/h, hex6 rises to 29° RMSE through a few outliers.
- **True position sd 0.06 cm with the prior still at 0.03:** hex6 4.2° and 7 % at 10 cm/h; tri3 12°.
- **True gain sd 5 % with the prior at 2 %:** hex6 3.8° at 10 cm/h, but 32° at 2 cm/h, as sec. 2.2 predicts.

## 5. Lesson from the soaked 4×4 capture (`real_capture.py`)

On real zero-flow data:

- The model without blur or loss fits a spurious u_x of 9.3 cm/h, with gains pushed to ±0.33 to absorb misfit.
- Adding blur still gives 10.5 cm/h.
- Letting u_y or λ float instead fits identically: u_x = 0.32 with either u_y = 10.6 or λ = 0.00098 /s. This is the λ ≡ |u|² degeneracy of sec. 2.2, appearing in data.

The tail decays faster than conduction alone allows, by the equivalent of about 10 cm/h of flow. Any method that reads flow from the decay or peak time inherits this error. The remaining misfit is 3.9 mK, about 3× the white-noise floor.

## 6. What a needle costs: blockage, wounds and shadows (`needle_cost.py`)

### Physics

Sap moves as Darcy flow through an anisotropic porous medium: q = −κ∇p, ∇·q = 0, with κ = diag(Kp, 1) in the grain frame. A needle plus its wound annulus (radius w/2) is impermeable.

Stretch the axial coordinate as X = x/√Kp. The problem becomes isotropic, and the disk becomes a flat plate normal to the flow. Its disturbance reaches |X| ≈ a, which is |x| ≈ a·√Kp in real space, both upstream and downstream. Potential flow is fore–aft symmetric, so there is no wake, but there is a stagnant strip on both sides.

- For a = 0.1 cm and 0.6 cm spacing, the strip reaches the next needle when Kp > 36.
- The flow halfway between two needles 0.6 cm apart on an axial line, relative to the free stream: 0.75 at Kp = 1, 0.05 at Kp = 30, 0.001 at Kp = 300 (`fig_needle_flow.png`).
- Large Kp is also the right limit when conduits are longer than the probe: a vessel cut by the drill is dead along its whole length.

I could not pin Kp for sapwood to a source I read. It is usually described as orders of magnitude, so it is swept here.

### Simulation

- The Darcy field is solved on x ∈ [−12, 13] cm. The tree sets the pressure gradient, not the probe.
- Conservative advection–diffusion uses Crank–Nicolson with dx = 0.02 cm.
- Needles are steel (r = 0.065 cm, no flow). The wound annulus has no flow and isotropic kT conductivity; this conductivity is an assumption.
- Fits use the analytic model (blur + loss). "Fixed" fits hold geometry at truth and isolate the physics bias. "Prior" fits use the usual priors.
- std_axial uses the project's 1-D `fit_pair`.
- Validation case: needles with wood properties and no wound. All layouts recover speed to within 0–2 % with fixed geometry (1-D fit_pair 1.3 %) and RMSE ≤ 0.44 mK.

### Results with fixed geometry, 10 cm/h, 2 mm wound unless stated

Fitted / true speed:

| layout | Kp = 1, vertical | Kp = 3000, vertical | Kp = 1, rotated 20° | Kp = 3000, rotated 20° | 3 mm wound, Kp 300, rotated 20° |
|---|---|---|---|---|---|
| std_axial (1-D) | 0.78 | 0.76 | 0.69 | 0.43 | 0.37 |
| tri3 | 0.84 | 0.77 | 0.86 | 0.58 | 0.58 |
| Y3 | 0.81 | 0.69 | 0.78 | 0.57 | 0.54 |
| hex6 | 0.83 | 0.77 | 0.86 | 0.41 | 0.44 |
| 2H_tri | 0.88 | 0.80 | 0.90 | 0.67 | 0.62 |
| dual3 | 0.84 | 0.73 | 0.85 | 0.70 | 0.65 |
| dual4 | 0.85 | 0.72 | 0.85 | 0.71 | 0.67 |

At the vertical install, direction errors are below 1° except Y3 at 2 cm/h. Fit RMSE values are in `fig_needle_cost.png`.

**Findings.**

- **Every needle costs speed.** The analytic model reads a wounded needle mostly as slower flow, not as misfit.
- **The cost depends on more than wound width.** It also depends on layout, on orientation relative to the flow, and on Kp, because of shadowing. A single scalar wound-correction curve cannot represent it.
- **More needles mean more shadows.** hex6 has the best CRB but the worst shadow bias once rotated.
- **Direction holds up better than speed.** hex6 and 2H_tri stay within 0.6°. dual4 is at most 1.7°, dual3 at most 1.9° (6.5° with 3 mm wounds), tri3 2–8° and Y3 up to 11°.
- **Systematic misfit is well above noise.** Single-heater layouts show 1–8 mK and dual layouts 2–8 mK vertical and 11–28 mK rotated. Wounds and non-uniform flow break the homogeneous model's reciprocity, which dual layouts rely on most.
- **With geometry free under priors, the fits fall apart.** The misfit leaks into positions and gains, giving dual-layout speeds 3–9× too high and angles 10–25° off; even single-heater layouts are erratic. The analytic model must gain a needle/wound term before it can be trusted in wood.

### Wound width (`results/needle_cost_wound_id.json`)

- A 1 mm change in wound width moves the curves by 13–157 mK.
- The analytic model's parameters absorb 99.5–99.99 % of that signature, mostly as speed, k and blur changes.
- The formal CRB on w with all analytic parameters free is 0.002–0.011 cm. It assumes the FD wound model is exactly right; the unexplained remainder (about 3 mK per mm) is the same size as other model errors seen in real data.
- The fitted blur c is not a clean wound indicator. It rises from 0.0011 to 0.0016 for small wounds and falls to 0.0006 at 3 mm.
- Estimating w needs a wound-aware forward model fitted alongside, and the dissection measurement afterwards as a check.

## 7. Recommendation

1. **Build dual-role needles.** Each needle carries a heater and a thermistor (or the usual 3 radial depths). Use 4 needles in the dual4 "Y" geometry: (0, 0), (s·cos30°, ±s/2), (0, s).
   - Pair directions are 30°, 90° and 150°, and no two needles share an axial line at a vertical install.
   - Use dual3 if 3 holes is the limit; it is the same hole count as East30 today.
   - Reasons: with known heater ratios they do not depend on gains (sec. 2.2); they reach about 4° and 7 % at 2 cm/h with 3–4 holes; they have the smallest shadow bias among the identifiable layouts.
   - The catch: they need the wound-aware model most, because reciprocity is their information source.
2. **Isotropic or prior-encoding layout?**
   - Choose the layout for isotropic identifiability: at least 3 directions. Axial + side layouts fail exactly where the installer is aiming.
   - Encode the "roughly vertical" prior in the install and in the shadow geometry, not in the layout.
   - Fit with the general model, and report the grain-vs-flow angle as a diagnostic.
3. **Spacing.** Use at least 0.6 cm and test 0.8–1.0 cm. Errors scale roughly as 1/s, and shadows reach the next needle only when a·√Kp > s. Signal at 1.0 cm is still about 0.47 °C at peak, about 100× the noise.
4. **Heaters.** For gain-free direction you need 3 or more heaters with known ratios. Two heaters (2H_tri) pin only one component.
5. **Do not trust the decay term or the T-max idea for speed.** Leave λ free.
6. **Next modelling step.** A needle/wound-aware forward model, either an FD surrogate or an analytic layer correction. Then repeat sec. 6 with geometry free.

## 8. Hardware and firmware needs

**Heaters**

- 3–4 independently switched heaters, one on at a time with a hardware interlock.
- The existing safety layers must cover every channel: one-pulse timer, IWDG, the TPL5010/SCR latch cutting all heaters, and thermistor interlock.
- Measure V and I for each pulse so heater energy is known to 0.1–0.2 %.
- Use a matched heater-element lot and calibrate heater amplitude ratios on the bench.

**Thermistors**

- One per needle per depth: 4 needles × 3 depths = 12 channels. This is well within the board's 48 multiplexed channels.
- Keep about 25 Hz per channel (or an equivalent averaged total). The information is in the slow curve, but the noise averaging needs the samples.

**Sequencing**

- Each heater fires in turn: 60 s baseline, 1 s pulse, 180 s recording, then recovery.
- A dual4 cycle is about 15 min. Overlapping pulses (the system is linear, so superposition fitting works) could shorten this; I have not analysed it.
- Record every channel during every window, including the firing needle's own thermistor (useful for heater-contact diagnosis).

**Firmware and logging**

- Log raw curves plus per-pulse energy. The SFK1 record needs one window per heater.
- Do the joint fit on the host, not the MCU.

## 9. Bench tests

1. **Agar or gel, zero flow, reciprocal pairs.** Measure heater amplitude ratios and relative thermistor gains, blur c, and positions from the a terms. Repeat with reinsertion. Target: ratio repeatability ≤ 0.2 %.
2. **Bath calibration of each thermistor channel's ΔT gain,** then a drift check after weeks in the field.
3. **Flow column.** Use saturated glass beads, sand, or a wood segment with pressure-driven flow and gravimetric flux. Rotate the probe 0/10/20/30/45/90° and compare fitted direction and speed with truth. This tests sec. 2.3 and the identifiability claims directly.
4. **Wound series in a flowing wood segment.** Drill 1.3, then 2, then 3 mm and repeat; compare with sec. 6. Section the wood afterwards to measure the wound.
5. **Shadow test.** Add a dummy needle directly upstream of a pair, then offset 0.3 cm laterally.
6. **Lab measurement of axial vs tangential permeability on cut sapwood blocks** of the target species, to set Kp.
7. **Off-grain test.** Use a block cut at a known angle, or a spiral-grain species, to check that the general model separates grain from flow.

## 10. Where this contradicts the literature

- **Vandegehuchte & Steppe 2012 (J. Exp. Bot. 63:2833)** report kT ≈ 54 ± 7.5 % of kL and argue that standard axial installs are largely unaffected by anisotropy. That is true only at perfect alignment. Sec. 2.3 shows anisotropy multiplies misalignment error by roughly R: at 20°, a 13 % underestimate instead of 5 %. The 1-D fit cannot detect it.
- **Isotropic multi-needle models** (Yang, Sakai & Jones 2013, WRR 49:5851, soil; Wang, Fan & Jones ~2018 in sapwood, which reported installation-angle-dependent errors; I could not access the full text) recover direction well but speed is biased by −12 to −19 % at R = 1.85 (sec. 4). This is consistent with the angle-dependent errors they report.
- **Wound corrections** (Swanson & Whitfield 1981, J. Exp. Bot. 32:221; Burgess et al. 2001):
  - They are a single function of wound width.
  - My FD shows the bias also depends on layout, orientation and permeability anisotropy, because impermeable needles cast axial shadows, which annulus-in-uniform-flow models omit.
  - For a 2 mm wound, the 1-D fit_pair ratio ranges from 0.78 to 0.43.
  - I could not access their coefficient tables, so I make no numeric comparison.
- **"Zero-flow offset" calibration and peak-time methods.** Sec. 2.2 shows these calibrate the gain gauge and rely on a no-loss assumption respectively. Nocturnal flow and real heat loss (sec. 5) make both assumptions, not measurements.
- **R = 1.85, Kp, and the wound conductivity** are assumptions here. R matters (sec. 2.3); Kp and wound conductivity are swept or flagged.

## 11. Sources and access

Read or abstract seen:

- Vandegehuchte & Steppe 2012 (New Phytol 196:306, Sapflow+; J. Exp. Bot. 63:2833)
- Yang, Sakai & Jones 2013
- Vité & Rudinsky 1959 (spiral ascent patterns)
- a 2026 EGU abstract reporting 1–1.5 helical tracer turns in spruce
- US 9638558 (in-situ spacing correction)

Not accessed in full; no numbers taken from them:

- Swanson & Whitfield 1981 and its corrigendum
- Burgess et al. 2001 coefficient table (connection refused)
- Kluitenberg & Ham 2004 (finite pulse)
- Wang, Fan & Jones (503 error)
- a Wood Handbook figure for the longitudinal/transverse conductivity ratio

## 12. Limitations

- The model is 2-D apart from sec. 2.6, with homogeneous wood apart from the wounds. Rays, rings and earlywood/latewood are not modelled.
- Gaussian needle blur is a second-order approximation.
- FD needles are the steel-plus-epoxy values listed above, and the wound conductivity is assumed.
- The Monte Carlo used the model without loss; the CRBs include loss.
- The FD fits are noise-free; the "prior" fits show the instability but are not a full error distribution.

## Reproduce

All scripts run from this directory:

    uv run --with numpy --with scipy --with pandas --with matplotlib python <script>

| script | output |
|---|---|
| `check_model.py` | model checks (prints) |
| `misalign_1d.py` | `results/misalign_1d.json`, `fig_misalign_1d.png` |
| `fisher.py` | CRBs without loss and spacing sweeps, `fig_fisher_*.png` |
| `fisher_needles.py` | CRB with blur and loss per needle, `fig_needles_info.png` |
| `fisher_gain.py` | gain vs heater-ratio CRB, `fig_gain_vs_heater.png` |
| `mc_fits.py [n]` | Monte Carlo fits (about 20 min on 16 cores) |
| `radial_flow.py` | radial-flow bounds and bias |
| `real_capture.py` | fits to the Aug 24 soaked capture (loads it via `../compression_research_20261024/common.py`) |
| `needle_cost.py` | Darcy + FD needle cost and wound identifiability, `fig_needle_flow.png`, `fig_needle_cost.png` (about 10 min) |

`results/fisher_angle.json` and `results/mc_trials.json` are regenerated and ignored by git because of their size.
