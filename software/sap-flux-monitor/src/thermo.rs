//! NTC thermistor conversion from raw ADC counts.
//!
//! The divider is: excitation rail -> NTC -> measured node -> 10 kΩ -> GND,
//! with the excitation rail sampled on its own ADC channel in the same
//! frame, so the conversion is ratiometric and raw counts can be used
//! directly in place of volts:
//!
//! ```text
//! R_ntc = 10000 * (exc - mid) / mid
//! l     = ln(R_ntc / 10000)
//! T_C   = 1 / (A3*l^3 + A2*l^2 + A1*l + A0) - 273.15
//! ```

/// Steinhart–Hart style coefficients (cubic in `ln(R/R25)`), lowest order
/// first: `1/T = A0 + A1*l + A2*l^2 + A3*l^3`.
pub const COEFFS: [f64; 4] = [0.003354, 2.565e-4, 2.325e-6, 1.024e-7];

/// Reference (25 °C) resistance of the NTC and of the bottom-leg resistor.
pub const R_REF_OHM: f64 = 10_000.0;

/// Excitation below this many counts means the rail is off or unreadable;
/// all channel conversions in that frame are invalid.
pub const EXC_MIN_COUNTS: i16 = 100;

/// Plausible resistance window; outside this the channel is treated as
/// open/short rather than a temperature.
pub const R_MIN_OHM: f64 = 200.0;
/// See [`R_MIN_OHM`].
pub const R_MAX_OHM: f64 = 500_000.0;

/// Plausible temperature window for a sap-flux probe.
pub const T_MIN_C: f64 = -55.0;
/// See [`T_MIN_C`].
pub const T_MAX_C: f64 = 150.0;

/// Result of converting one thermistor channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelReading {
    /// Raw ADC counts as received.
    pub raw: i16,
    /// Divider resistance, when computable and plausible.
    pub resistance_ohm: Option<f64>,
    /// Temperature, when the resistance maps to a plausible temperature.
    pub temp_c: Option<f64>,
}

impl ChannelReading {
    /// True when this channel produced a usable temperature.
    pub fn valid(&self) -> bool {
        self.temp_c.is_some()
    }

    fn invalid(raw: i16) -> Self {
        Self {
            raw,
            resistance_ohm: None,
            temp_c: None,
        }
    }
}

/// Convert one channel. Never returns NaN/inf: nonsensical ratios, zero or
/// negative readings, and open/short channels come back as `None` fields.
pub fn convert_channel(exc_raw: i16, mid_raw: i16) -> ChannelReading {
    if exc_raw < EXC_MIN_COUNTS || mid_raw <= 0 || mid_raw >= exc_raw {
        return ChannelReading::invalid(mid_raw);
    }
    let exc = exc_raw as f64;
    let mid = mid_raw as f64;
    let r = R_REF_OHM * (exc - mid) / mid;
    if !r.is_finite() || !(R_MIN_OHM..=R_MAX_OHM).contains(&r) {
        return ChannelReading::invalid(mid_raw);
    }
    let l = (r / R_REF_OHM).ln();
    let inv_t = COEFFS[0] + l * (COEFFS[1] + l * (COEFFS[2] + l * COEFFS[3]));
    if inv_t <= 0.0 || !inv_t.is_finite() {
        return ChannelReading::invalid(mid_raw);
    }
    let t_c = 1.0 / inv_t - 273.15;
    if !t_c.is_finite() || !(T_MIN_C..=T_MAX_C).contains(&t_c) {
        return ChannelReading::invalid(mid_raw);
    }
    ChannelReading {
        raw: mid_raw,
        resistance_ohm: Some(r),
        temp_c: Some(t_c),
    }
}

/// Convert every channel of a sample (6 or 12). `adc_error` forces every
/// channel invalid (raw values are preserved for logging).
pub fn convert_sample<const N: usize>(
    exc_raw: i16,
    thermistor_raw: &[i16; N],
    adc_error: bool,
) -> [ChannelReading; N] {
    thermistor_raw.map(|mid| {
        if adc_error {
            ChannelReading::invalid(mid)
        } else {
            convert_channel(exc_raw, mid)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mid counts that put the divider exactly at resistance `r`.
    fn mid_for_r(exc: i16, r: f64) -> i16 {
        (exc as f64 * R_REF_OHM / (R_REF_OHM + r)).round() as i16
    }

    #[test]
    fn midpoint_is_25c() {
        // mid = exc/2  =>  R = 10k  =>  l = 0  =>  T = 1/0.003354 - 273.15.
        let reading = convert_channel(26000, 13000);
        let r = reading.resistance_ohm.unwrap();
        assert!((r - 10_000.0).abs() < 1e-9, "r = {r}");
        let t = reading.temp_c.unwrap();
        let expected = 1.0 / 0.003354 - 273.15; // 25.0 °C for these coeffs
        assert!((t - expected).abs() < 1e-9, "t = {t}");
        assert!((t - 25.0).abs() < 0.01);
    }

    #[test]
    fn warmer_means_lower_resistance() {
        let cold = convert_channel(26000, mid_for_r(26000, 20_000.0));
        let warm = convert_channel(26000, mid_for_r(26000, 5_000.0));
        assert!(cold.temp_c.unwrap() < 25.0);
        assert!(warm.temp_c.unwrap() > 25.0);
        // Sanity: a 10k NTC at ~5k should be roughly 40-45 °C.
        let t = warm.temp_c.unwrap();
        assert!((35.0..50.0).contains(&t), "t = {t}");
    }

    #[test]
    fn ratiometric_independence_from_excitation_level() {
        // Same ratio at different excitation levels gives the same T.
        let a = convert_channel(26000, 13000).temp_c.unwrap();
        let b = convert_channel(20000, 10000).temp_c.unwrap();
        assert!((a - b).abs() < 0.02, "{a} vs {b}");
    }

    #[test]
    fn open_channel_invalid() {
        // Open NTC: measured node pulled to ground, raw ~ 0.
        assert!(!convert_channel(26000, 0).valid());
        assert!(!convert_channel(26000, -3).valid());
        // Tiny counts imply resistance above R_MAX -> invalid.
        assert!(!convert_channel(26000, 1).valid());
    }

    #[test]
    fn short_channel_invalid() {
        // Shorted NTC: node at excitation, R = 0.
        assert!(!convert_channel(26000, 26000).valid());
        // Above excitation is nonsense.
        assert!(!convert_channel(26000, 30000).valid());
        // Just below excitation implies R below R_MIN -> invalid.
        assert!(!convert_channel(26000, 25999).valid());
    }

    #[test]
    fn dead_excitation_invalidates_everything() {
        assert!(!convert_channel(0, 13000).valid());
        assert!(!convert_channel(50, 25).valid());
        assert!(!convert_channel(-100, -200).valid());
        let all = convert_sample(0, &[13000; 6], false);
        assert!(all.iter().all(|c| !c.valid()));
    }

    #[test]
    fn no_nan_or_inf_ever() {
        for exc in [-32768i16, -1, 0, 1, 100, 1000, 26000, 32767] {
            for mid in [-32768i16, -1, 0, 1, 100, 12345, 26000, 32767] {
                let r = convert_channel(exc, mid);
                if let Some(res) = r.resistance_ohm {
                    assert!(res.is_finite());
                }
                if let Some(t) = r.temp_c {
                    assert!(t.is_finite());
                    assert!((T_MIN_C..=T_MAX_C).contains(&t));
                }
            }
        }
    }

    #[test]
    fn adc_error_flag_invalidates_channels() {
        let ok = convert_sample(26000, &[13000; 6], false);
        assert!(ok.iter().all(|c| c.valid()));
        let bad = convert_sample(26000, &[13000; 6], true);
        assert!(bad.iter().all(|c| !c.valid()));
        // Raw values still preserved for logging.
        assert!(bad.iter().all(|c| c.raw == 13000));
    }
}
