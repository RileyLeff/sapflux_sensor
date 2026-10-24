#!/usr/bin/env python3
"""Fit the derivative/full-trace Marshall model to a SapFlux capture.

The fit deliberately keeps the two information channels visible:

1. alpha(t) = log(downstream/upstream) = zeta - epsilon*tau/t_eff
2. the finite-pulse common curve resolves tau and absolute amplitude

Absolute amplitude is then translated to xbar only under explicit heater-energy,
effective-heater-length, volumetric-heat-capacity, and unit-gain assumptions.
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict, dataclass
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
from scipy.optimize import minimize_scalar


DEFAULT_CAPTURE = Path(
    "/Users/rileyleff/Documents/hard/saptest1/software/sap-flux-monitor/"
    "captures/session_20260824_203902/samples.csv"
)
DEFAULT_OUTPUT = Path(
    "/Users/rileyleff/Documents/hard/saptest1/analysis/outputs/4x4_pulse_20260824"
)

# User-confirmed wiring for the August sensor head. T1/far tip is broken.
PAIRS = {
    "middle": (2, 5),  # far/downstream T2, near/upstream T5
    "head": (3, 6),  # far/downstream T3, near/upstream T6
}
PAIR_COLORS = {"middle": "#166534", "head": "#7C3AED"}
DOWNSTREAM = "#E69F00"
UPSTREAM = "#3B82F6"
INK = "#202428"
MUTED = "#667078"


@dataclass(frozen=True)
class FitResult:
    pair: str
    downstream_channel: int
    upstream_channel: int
    samples_alpha: int
    alpha_intercept_zeta: float
    alpha_inverse_time_slope_s: float
    epsilon_tau_s: float
    alpha_rmse: float
    alpha_r_squared: float
    tau_s: float
    epsilon: float
    downstream_amplitude_c: float
    upstream_amplitude_c: float
    common_amplitude_c: float
    trace_rmse_c: float
    alpha_60_80: float
    stock_velocity_cm_hr: float
    nominal_xbar_cm: float
    nominal_xd_cm: float
    nominal_xu_cm: float
    nominal_k_cm2_s: float
    nominal_velocity_cm_hr: float
    amplitude_xbar_cm: float
    amplitude_xd_cm: float
    amplitude_xu_cm: float
    amplitude_k_cm2_s: float
    amplitude_velocity_cm_hr: float
    rho_c_for_nominal_xbar_j_m3_k: float


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", type=Path, default=DEFAULT_CAPTURE)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--run-id", type=int)
    parser.add_argument("--nominal-xbar-cm", type=float, default=0.6)
    parser.add_argument("--heater-voltage-v", type=float, default=25.6)
    parser.add_argument("--heater-resistance-ohm", type=float, default=42.0)
    parser.add_argument("--heater-effective-length-m", type=float, default=0.02)
    parser.add_argument("--rho-c-j-m3-k", type=float, default=2.5e6)
    parser.add_argument("--alpha-start-s", type=float, default=8.0)
    parser.add_argument("--fit-end-s", type=float, default=120.0)
    parser.add_argument("--minimum-rise-c", type=float, default=0.05)
    return parser.parse_args()


def load_capture(
    path: Path, run_id: int | None
) -> tuple[pd.DataFrame, int, float, float, float, dict[int, float]]:
    frame = pd.read_csv(path)
    active = frame[frame["heater_active"].astype(bool)]
    if active.empty:
        raise ValueError(f"No heater-active samples in {path}")
    if run_id is None:
        run_id = int(active.groupby("run_id").size().idxmax())
    frame = frame[frame["run_id"].eq(run_id)].copy()
    active = frame[frame["heater_active"].astype(bool)]
    if active.empty:
        raise ValueError(f"Run {run_id} has no heater-active samples")

    onset_us = float(active.iloc[0]["monotonic_us"])
    frame["time_s"] = (frame["monotonic_us"].to_numpy(float) - onset_us) / 1e6
    sample_period_s = float(np.median(np.diff(frame["monotonic_us"]))) / 1e6
    pulse_duration_s = (
        float(active.iloc[-1]["monotonic_us"] - active.iloc[0]["monotonic_us"])
        / 1e6
        + sample_period_s
    )
    voltage_adc_estimate = (
        float(active["v24_raw"].mean()) * 3.3 / 4095.0 * 11.0
    )

    baseline_noise: dict[int, float] = {}
    for channel in sorted({item for pair in PAIRS.values() for item in pair}):
        valid = (
            frame["time_s"].between(-20.0, -1.0)
            & frame[f"t{channel}_valid"].astype(bool)
            & np.isfinite(frame[f"t{channel}_temp_c"])
        )
        coefficients = np.polyfit(
            frame.loc[valid, "time_s"],
            frame.loc[valid, f"t{channel}_temp_c"],
            1,
        )
        residual = frame.loc[valid, f"t{channel}_temp_c"] - np.polyval(
            coefficients, frame.loc[valid, "time_s"]
        )
        baseline_noise[channel] = float(
            np.median(np.abs(residual - np.median(residual))) / 0.6744897501960817
        )
        frame[f"t{channel}_rise_c"] = frame[f"t{channel}_temp_c"] - np.polyval(
            coefficients, frame["time_s"]
        )
    return (
        frame,
        run_id,
        sample_period_s,
        pulse_duration_s,
        voltage_adc_estimate,
        baseline_noise,
    )


def finite_pulse_response(
    time_s: np.ndarray,
    zeta: float,
    tau_s: float,
    epsilon: float,
    side: int,
    pulse_duration_s: float,
    nodes: int = 32,
) -> np.ndarray:
    """Dimensionless finite rectangular line-source response."""

    quadrature_x, quadrature_w = np.polynomial.legendre.leggauss(nodes)
    source_time = 0.5 * pulse_duration_s * (quadrature_x + 1.0)
    weights = 0.5 * quadrature_w
    elapsed = np.asarray(time_s, dtype=float)[:, None] - source_time[None, :]
    valid = elapsed > 0.0
    ratio = np.where(valid, elapsed / tau_s, 1.0)
    distance = 1.0 + side * epsilon
    signed_distance = (
        distance - zeta * ratio if side > 0 else distance + zeta * ratio
    )
    response = np.where(
        valid,
        np.exp(-(signed_distance**2) / (4.0 * ratio)) / ratio,
        0.0,
    )
    return np.sum(response * weights[None, :], axis=1)


def profiled_amplitude(response: np.ndarray, observed: np.ndarray) -> float:
    return max(0.0, float(np.dot(response, observed) / np.dot(response, response)))


def fit_pair(
    frame: pd.DataFrame,
    pair_name: str,
    downstream_channel: int,
    upstream_channel: int,
    pulse_duration_s: float,
    args: argparse.Namespace,
) -> tuple[FitResult, pd.DataFrame]:
    downstream = frame[f"t{downstream_channel}_rise_c"]
    upstream = frame[f"t{upstream_channel}_rise_c"]
    alpha_mask = (
        frame["time_s"].between(args.alpha_start_s, args.fit_end_s)
        & downstream.gt(args.minimum_rise_c)
        & upstream.gt(args.minimum_rise_c)
    )
    alpha_time = frame.loc[alpha_mask, "time_s"].to_numpy(float)
    alpha_observed = np.log(
        downstream[alpha_mask].to_numpy(float)
        / upstream[alpha_mask].to_numpy(float)
    )
    effective_time = alpha_time - pulse_duration_s / 2.0
    design = np.column_stack((np.ones(len(alpha_time)), 1.0 / effective_time))
    zeta, inverse_time_slope = np.linalg.lstsq(
        design, alpha_observed, rcond=None
    )[0]
    alpha_predicted = design @ np.array([zeta, inverse_time_slope])
    alpha_residual = alpha_observed - alpha_predicted
    alpha_r_squared = 1.0 - float(np.dot(alpha_residual, alpha_residual)) / float(
        np.dot(alpha_observed - alpha_observed.mean(), alpha_observed - alpha_observed.mean())
    )

    trace_mask = frame["time_s"].between(5.0, args.fit_end_s)
    trace_time = frame.loc[trace_mask, "time_s"].to_numpy(float)
    downstream_observed = downstream[trace_mask].to_numpy(float)
    upstream_observed = upstream[trace_mask].to_numpy(float)
    epsilon_tau = -float(inverse_time_slope)

    def objective(log_tau: float) -> float:
        tau = float(np.exp(log_tau))
        epsilon = epsilon_tau / tau
        if abs(epsilon) >= 0.45:
            return 1e6
        downstream_response = finite_pulse_response(
            trace_time, zeta, tau, epsilon, 1, pulse_duration_s
        )
        upstream_response = finite_pulse_response(
            trace_time, zeta, tau, epsilon, -1, pulse_duration_s
        )
        downstream_amplitude = profiled_amplitude(
            downstream_response, downstream_observed
        )
        upstream_amplitude = profiled_amplitude(upstream_response, upstream_observed)
        residual = np.concatenate(
            (
                downstream_amplitude * downstream_response - downstream_observed,
                upstream_amplitude * upstream_response - upstream_observed,
            )
        )
        return float(np.mean(residual**2))

    lower_tau = max(20.0, abs(epsilon_tau) / 0.44)
    optimization = minimize_scalar(
        objective,
        bounds=(np.log(lower_tau), np.log(1000.0)),
        method="bounded",
        options={"xatol": 1e-10},
    )
    tau = float(np.exp(optimization.x))
    epsilon = epsilon_tau / tau
    downstream_response = finite_pulse_response(
        trace_time, zeta, tau, epsilon, 1, pulse_duration_s
    )
    upstream_response = finite_pulse_response(
        trace_time, zeta, tau, epsilon, -1, pulse_duration_s
    )
    downstream_amplitude = profiled_amplitude(
        downstream_response, downstream_observed
    )
    upstream_amplitude = profiled_amplitude(upstream_response, upstream_observed)
    common_amplitude = float(np.sqrt(downstream_amplitude * upstream_amplitude))

    energy_per_length = (
        args.heater_voltage_v**2
        / args.heater_resistance_ohm
        * pulse_duration_s
        / args.heater_effective_length_m
    )
    amplitude_xbar_cm = 100.0 * np.sqrt(
        energy_per_length / (4.0 * np.pi * args.rho_c_j_m3_k * common_amplitude)
    )

    window = frame["time_s"].between(60.0, 80.0)
    alpha_60_80 = float(
        np.log(downstream[window].mean() / upstream[window].mean())
    )
    stock_velocity = (
        3600.0 * 0.0025 / args.nominal_xbar_cm * alpha_60_80
    )

    def physical(scale_cm: float) -> tuple[float, float, float, float]:
        xd = scale_cm * (1.0 + epsilon)
        xu = scale_cm * (1.0 - epsilon)
        diffusivity = scale_cm**2 / tau
        velocity = 3600.0 * scale_cm * zeta / tau
        return xd, xu, diffusivity, velocity

    nominal_xd, nominal_xu, nominal_k, nominal_velocity = physical(
        args.nominal_xbar_cm
    )
    amplitude_xd, amplitude_xu, amplitude_k, amplitude_velocity = physical(
        amplitude_xbar_cm
    )
    rho_c_for_nominal = args.rho_c_j_m3_k * (
        amplitude_xbar_cm / args.nominal_xbar_cm
    ) ** 2

    result = FitResult(
        pair=pair_name,
        downstream_channel=downstream_channel,
        upstream_channel=upstream_channel,
        samples_alpha=len(alpha_time),
        alpha_intercept_zeta=float(zeta),
        alpha_inverse_time_slope_s=float(inverse_time_slope),
        epsilon_tau_s=epsilon_tau,
        alpha_rmse=float(np.sqrt(np.mean(alpha_residual**2))),
        alpha_r_squared=alpha_r_squared,
        tau_s=tau,
        epsilon=epsilon,
        downstream_amplitude_c=downstream_amplitude,
        upstream_amplitude_c=upstream_amplitude,
        common_amplitude_c=common_amplitude,
        trace_rmse_c=float(np.sqrt(optimization.fun)),
        alpha_60_80=alpha_60_80,
        stock_velocity_cm_hr=stock_velocity,
        nominal_xbar_cm=args.nominal_xbar_cm,
        nominal_xd_cm=nominal_xd,
        nominal_xu_cm=nominal_xu,
        nominal_k_cm2_s=nominal_k,
        nominal_velocity_cm_hr=nominal_velocity,
        amplitude_xbar_cm=float(amplitude_xbar_cm),
        amplitude_xd_cm=amplitude_xd,
        amplitude_xu_cm=amplitude_xu,
        amplitude_k_cm2_s=amplitude_k,
        amplitude_velocity_cm_hr=amplitude_velocity,
        rho_c_for_nominal_xbar_j_m3_k=float(rho_c_for_nominal),
    )
    fitted = pd.DataFrame(
        {
            "pair": pair_name,
            "time_s": trace_time,
            "downstream_observed_c": downstream_observed,
            "upstream_observed_c": upstream_observed,
            "downstream_fitted_c": downstream_amplitude * downstream_response,
            "upstream_fitted_c": upstream_amplitude * upstream_response,
        }
    )
    return result, fitted


def sampling_sensitivity(
    frame: pd.DataFrame, pulse_duration_s: float, args: argparse.Namespace
) -> pd.DataFrame:
    sample_period = float(np.median(np.diff(frame["time_s"])))
    rows: list[dict[str, float | int | str]] = []
    for pair_name, (downstream_channel, upstream_channel) in PAIRS.items():
        downstream = frame[f"t{downstream_channel}_rise_c"]
        upstream = frame[f"t{upstream_channel}_rise_c"]
        mask = (
            frame["time_s"].between(args.alpha_start_s, args.fit_end_s)
            & downstream.gt(args.minimum_rise_c)
            & upstream.gt(args.minimum_rise_c)
        )
        time = frame.loc[mask, "time_s"].to_numpy(float)
        alpha = np.log(
            downstream[mask].to_numpy(float) / upstream[mask].to_numpy(float)
        )
        for frequency in (1, 2, 5, 10, 25, 50):
            step = max(1, round((1.0 / sample_period) / frequency))
            for phase in range(step):
                selected_time = time[phase::step]
                selected_alpha = alpha[phase::step]
                effective = selected_time - pulse_duration_s / 2.0
                design = np.column_stack(
                    (np.ones(len(selected_time)), 1.0 / effective)
                )
                intercept, slope = np.linalg.lstsq(
                    design, selected_alpha, rcond=None
                )[0]
                rows.append(
                    {
                        "pair": pair_name,
                        "frequency_hz": frequency,
                        "phase": phase,
                        "samples": len(selected_time),
                        "zeta": intercept,
                        "inverse_time_slope_s": slope,
                    }
                )
    return pd.DataFrame(rows)


def make_plot(
    frame: pd.DataFrame,
    results: list[FitResult],
    fitted: pd.DataFrame,
    sensitivity: pd.DataFrame,
    pulse_duration_s: float,
    output: Path,
) -> None:
    fig, axes = plt.subplots(2, 2, figsize=(14.5, 9.5))
    for axis, result in zip(axes[0], results, strict=True):
        pair_fit = fitted[fitted["pair"].eq(result.pair)]
        axis.scatter(
            pair_fit["time_s"][::8],
            pair_fit["downstream_observed_c"][::8],
            s=9,
            alpha=0.28,
            color=DOWNSTREAM,
            label="Downstream observed",
        )
        axis.scatter(
            pair_fit["time_s"][::8],
            pair_fit["upstream_observed_c"][::8],
            s=9,
            alpha=0.28,
            color=UPSTREAM,
            label="Upstream observed",
        )
        axis.plot(
            pair_fit["time_s"],
            pair_fit["downstream_fitted_c"],
            lw=2.1,
            color=DOWNSTREAM,
        )
        axis.plot(
            pair_fit["time_s"],
            pair_fit["upstream_fitted_c"],
            lw=2.1,
            color=UPSTREAM,
        )
        axis.set_title(
            f"{result.pair.title()} pair: finite-pulse fit\n"
            f"tau={result.tau_s:.1f} s, epsilon={result.epsilon:.4f}, "
            f"RMSE={result.trace_rmse_c:.3f} °C"
        )
        axis.set_xlabel("Seconds from heater onset")
        axis.set_ylabel("Baseline-detrended rise (°C)")
        axis.grid(alpha=0.2)
    axes[0, 0].legend(frameon=False)

    alpha_axis = axes[1, 0]
    for result in results:
        downstream = frame[f"t{result.downstream_channel}_rise_c"]
        upstream = frame[f"t{result.upstream_channel}_rise_c"]
        mask = (
            frame["time_s"].between(8.0, 120.0)
            & downstream.gt(0.05)
            & upstream.gt(0.05)
        )
        effective = frame.loc[mask, "time_s"].to_numpy(float) - pulse_duration_s / 2
        alpha = np.log(
            downstream[mask].to_numpy(float) / upstream[mask].to_numpy(float)
        )
        inverse_time = 1.0 / effective
        order = np.argsort(inverse_time)
        color = PAIR_COLORS[result.pair]
        alpha_axis.scatter(inverse_time[::12], alpha[::12], s=8, alpha=0.25, color=color)
        alpha_axis.plot(
            inverse_time[order],
            result.alpha_intercept_zeta
            + result.alpha_inverse_time_slope_s * inverse_time[order],
            color=color,
            lw=2.4,
            label=f"{result.pair}: R²={result.alpha_r_squared:.3f}",
        )
    alpha_axis.set_title("The derivative channel is extremely clean")
    alpha_axis.set_xlabel("Inverse effective time, 1/t (s⁻¹)")
    alpha_axis.set_ylabel("alpha(t) = log(ΔTdown/ΔTup)")
    alpha_axis.grid(alpha=0.2)
    alpha_axis.legend(frameon=False)

    rate_axis = axes[1, 1]
    summary = (
        sensitivity.groupby(["pair", "frequency_hz"])["inverse_time_slope_s"]
        .agg(["min", "median", "max"])
        .reset_index()
    )
    for pair_name, group in summary.groupby("pair"):
        color = PAIR_COLORS[pair_name]
        rate_axis.plot(
            group["frequency_hz"],
            -group["median"],
            marker="o",
            lw=2.0,
            color=color,
            label=pair_name,
        )
        rate_axis.fill_between(
            group["frequency_hz"],
            -group["max"],
            -group["min"],
            color=color,
            alpha=0.16,
        )
    rate_axis.set_xscale("log")
    rate_axis.set_xticks([1, 2, 5, 10, 25, 50], ["1", "2", "5", "10", "25", "50"])
    rate_axis.set_title("Slope stability under phase-shifted downsampling")
    rate_axis.set_xlabel("Retained sampling rate (Hz)")
    rate_axis.set_ylabel("epsilon × tau (s)")
    rate_axis.grid(alpha=0.2)
    rate_axis.legend(frameon=False)

    fig.suptitle("August 24 soaked-4×4 pulse: derivative plus full-curve fit", fontsize=18, y=0.99)
    fig.text(
        0.99,
        0.012,
        "Absolute xbar requires heater energy, effective source length, wood rho-c, and gain assumptions.",
        ha="right",
        color=MUTED,
        fontsize=9,
    )
    fig.tight_layout(rect=(0, 0.025, 1, 0.965))
    fig.savefig(output / "4x4_pulse_model_fit.png", dpi=220, bbox_inches="tight")
    fig.savefig(output / "4x4_pulse_model_fit.svg", bbox_inches="tight")
    plt.close(fig)


def write_report(
    args: argparse.Namespace,
    run_id: int,
    sample_period_s: float,
    pulse_duration_s: float,
    voltage_adc_estimate: float,
    baseline_noise: dict[int, float],
    results: list[FitResult],
) -> None:
    lines = [
        "# August 24 soaked-4x4 pulse fit",
        "",
        f"- run ID: `{run_id}`",
        f"- measured sampling rate: `{1.0 / sample_period_s:.3f} Hz`",
        f"- heater-active duration: `{pulse_duration_s:.6f} s`",
        f"- V24 ADC estimate during pulse: `{voltage_adc_estimate:.3f} V`",
        f"- amplitude calculation voltage assumption: `{args.heater_voltage_v:.3f} V`",
        f"- heater resistance / effective length: `{args.heater_resistance_ohm:.1f} ohm` / `{100 * args.heater_effective_length_m:.1f} cm`",
        f"- volumetric heat-capacity assumption: `{args.rho_c_j_m3_k / 1e6:.3f} MJ m^-3 K^-1`",
        f"- median robust pre-pulse noise: `{np.median(list(baseline_noise.values())):.4f} C`",
        "",
        "The fitted identity is `alpha(t) = zeta - epsilon*tau/t_eff`. The common finite-pulse curve resolves tau; epsilon follows from the fitted inverse-time slope. Physical K and V are shown both at nominal xbar and at the amplitude-derived xbar under the assumptions above.",
        "",
        "| pair | R2 alpha | zeta | epsilon*tau (s) | tau (s) | epsilon | delta at 6 mm (mm) | K at 6 mm (cm2/s) | V at 6 mm (cm/h) | amplitude xbar (cm) |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for result in results:
        lines.append(
            f"| {result.pair} | {result.alpha_r_squared:.4f} | {result.alpha_intercept_zeta:+.5f} | "
            f"{result.epsilon_tau_s:.4f} | {result.tau_s:.2f} | {result.epsilon:.5f} | "
            f"{20 * result.nominal_xbar_cm * result.epsilon:.3f} | "
            f"{result.nominal_k_cm2_s:.6f} | {result.nominal_velocity_cm_hr:+.3f} | "
            f"{result.amplitude_xbar_cm:.3f} |"
        )
    lines.extend(
        [
            "",
            "Amplitude-derived xbar scales as `V / sqrt(R * L * rho_c * gain)`. Treat it as a sensitivity result until heater resistance, effective source length, wood water content/heat capacity, and channel gain are measured.",
            "",
        ]
    )
    (args.output / "README.md").write_text("\n".join(lines))


def main() -> None:
    args = parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    (
        frame,
        run_id,
        sample_period_s,
        pulse_duration_s,
        voltage_adc_estimate,
        baseline_noise,
    ) = load_capture(args.capture, args.run_id)

    results: list[FitResult] = []
    fit_frames: list[pd.DataFrame] = []
    for pair_name, (downstream_channel, upstream_channel) in PAIRS.items():
        result, fitted = fit_pair(
            frame,
            pair_name,
            downstream_channel,
            upstream_channel,
            pulse_duration_s,
            args,
        )
        results.append(result)
        fit_frames.append(fitted)
    fitted = pd.concat(fit_frames, ignore_index=True)
    sensitivity = sampling_sensitivity(frame, pulse_duration_s, args)

    pd.DataFrame([asdict(result) for result in results]).to_csv(
        args.output / "pair_fit_summary.csv", index=False
    )
    fitted.to_csv(args.output / "fitted_curves.csv", index=False)
    sensitivity.to_csv(args.output / "sampling_rate_sensitivity.csv", index=False)
    payload = {
        "capture": str(args.capture),
        "run_id": run_id,
        "sample_frequency_hz": 1.0 / sample_period_s,
        "pulse_duration_s": pulse_duration_s,
        "v24_adc_estimate_during_pulse_v": voltage_adc_estimate,
        "assumptions": {
            "nominal_xbar_cm": args.nominal_xbar_cm,
            "heater_voltage_v": args.heater_voltage_v,
            "heater_resistance_ohm": args.heater_resistance_ohm,
            "heater_effective_length_m": args.heater_effective_length_m,
            "rho_c_j_m3_k": args.rho_c_j_m3_k,
        },
        "baseline_noise_c": baseline_noise,
        "pairs": [asdict(result) for result in results],
    }
    (args.output / "fit_summary.json").write_text(json.dumps(payload, indent=2) + "\n")
    make_plot(frame, results, fitted, sensitivity, pulse_duration_s, args.output)
    write_report(
        args,
        run_id,
        sample_period_s,
        pulse_duration_s,
        voltage_adc_estimate,
        baseline_noise,
        results,
    )
    print(pd.DataFrame([asdict(result) for result in results]).to_string(index=False))
    print(f"\nWrote {args.output}")


if __name__ == "__main__":
    main()
