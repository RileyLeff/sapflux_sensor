//! sap-flux field server: board-frame and science-window decoding.

pub mod board;
pub mod window;

/// Wall clock, Unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Format Unix ms as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn fmt_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_format() {
        // 1787618352639 ms = 2026-08-25T00:39:12.639Z (computed with Python datetime).
        assert_eq!(super::fmt_utc(1_787_618_352_639), "2026-08-25T00:39:12.639Z");
        assert_eq!(super::fmt_utc(0), "1970-01-01T00:00:00.000Z");
    }
}
