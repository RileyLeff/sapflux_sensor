# August 24 soaked-4x4 pulse fit

- run ID: `1787618451`
- measured sampling rate: `49.798 Hz`
- heater-active duration: `0.987039 s`
- V24 ADC estimate during pulse: `26.511 V`
- amplitude calculation voltage assumption: `25.600 V`
- heater resistance / effective length: `42.0 ohm` / `2.0 cm`
- volumetric heat-capacity assumption: `2.500 MJ m^-3 K^-1`
- median robust pre-pulse noise: `0.0061 C`

The fitted identity is `alpha(t) = zeta - epsilon*tau/t_eff`. The common finite-pulse curve resolves tau; epsilon follows from the fitted inverse-time slope. Physical K and V are shown both at nominal xbar and at the amplitude-derived xbar under the assumptions above.

| pair | R2 alpha | zeta | epsilon*tau (s) | tau (s) | epsilon | delta at 6 mm (mm) | K at 6 mm (cm2/s) | V at 6 mm (cm/h) | amplitude xbar (cm) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| middle | 0.9963 | +0.03124 | 6.9114 | 152.68 | 0.04527 | 0.543 | 0.002358 | +0.442 | 0.517 |
| head | 0.9777 | -0.01485 | 3.8003 | 143.79 | 0.02643 | 0.317 | 0.002504 | -0.223 | 0.551 |

Amplitude-derived xbar scales as `V / sqrt(R * L * rho_c * gain)`. Treat it as a sensitivity result until heater resistance, effective source length, wood water content/heat capacity, and channel gain are measured.
