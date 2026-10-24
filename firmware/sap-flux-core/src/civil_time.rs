const MILLIS_PER_DAY: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UtcDateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    /// ISO weekday: Monday = 1, Sunday = 7.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millisecond: u16,
}

pub fn unix_ms_to_datetime(unix_ms: i64) -> Option<UtcDateTime> {
    let days = unix_ms.div_euclid(MILLIS_PER_DAY);
    let millis_in_day = unix_ms.rem_euclid(MILLIS_PER_DAY);
    let (year, month, day) = civil_from_days(days)?;
    if !(2000..=2099).contains(&year) {
        return None;
    }

    let hour = millis_in_day / 3_600_000;
    let minute = (millis_in_day % 3_600_000) / 60_000;
    let second = (millis_in_day % 60_000) / 1_000;
    let millisecond = millis_in_day % 1_000;
    let weekday = (days + 3).rem_euclid(7) + 1;

    Some(UtcDateTime {
        year: year as u16,
        month: month as u8,
        day: day as u8,
        weekday: weekday as u8,
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        millisecond: millisecond as u16,
    })
}

pub fn datetime_to_unix_ms(value: UtcDateTime) -> Option<i64> {
    if !(2000..=2099).contains(&value.year)
        || !(1..=12).contains(&value.month)
        || !(1..=days_in_month(value.year, value.month)).contains(&value.day)
        || !(1..=7).contains(&value.weekday)
        || value.hour > 23
        || value.minute > 59
        || value.second > 59
        || value.millisecond > 999
    {
        return None;
    }

    let days = days_from_civil(value.year as i64, value.month as i64, value.day as i64);
    let calculated_weekday = (days + 3).rem_euclid(7) + 1;
    if value.weekday as i64 != calculated_weekday {
        return None;
    }

    Some(
        days * MILLIS_PER_DAY
            + value.hour as i64 * 3_600_000
            + value.minute as i64 * 60_000
            + value.second as i64 * 1_000
            + value.millisecond as i64,
    )
}

pub const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

pub const fn is_leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> Option<(i64, i64, i64)> {
    let shifted = days.checked_add(719_468)?;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some((year, month, day))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(
        year: u16,
        month: u8,
        day: u8,
        weekday: u8,
        hour: u8,
        minute: u8,
        second: u8,
        millisecond: u16,
    ) -> UtcDateTime {
        UtcDateTime {
            year,
            month,
            day,
            weekday,
            hour,
            minute,
            second,
            millisecond,
        }
    }

    #[test]
    fn known_dates_round_trip() {
        let cases = [
            (946_684_800_000, dt(2000, 1, 1, 6, 0, 0, 0, 0)),
            (951_827_696_789, dt(2000, 2, 29, 2, 12, 34, 56, 789)),
            (1_735_689_599_999, dt(2024, 12, 31, 2, 23, 59, 59, 999)),
            (4_102_444_799_999, dt(2099, 12, 31, 4, 23, 59, 59, 999)),
        ];

        for (unix_ms, expected) in cases {
            assert_eq!(unix_ms_to_datetime(unix_ms), Some(expected));
            assert_eq!(datetime_to_unix_ms(expected), Some(unix_ms));
        }
    }

    #[test]
    fn rejects_dates_outside_stm32_calendar_window() {
        assert_eq!(unix_ms_to_datetime(946_684_799_999), None);
        assert_eq!(unix_ms_to_datetime(4_102_444_800_000), None);
    }

    #[test]
    fn rejects_invalid_calendar_fields_and_weekday() {
        assert_eq!(datetime_to_unix_ms(dt(2025, 2, 29, 6, 0, 0, 0, 0)), None);
        assert_eq!(datetime_to_unix_ms(dt(2024, 2, 29, 5, 0, 0, 0, 0)), None);
        assert_eq!(datetime_to_unix_ms(dt(2024, 2, 29, 4, 24, 0, 0, 0)), None);
    }

    #[test]
    fn every_day_in_supported_range_round_trips() {
        let mut unix_ms = 946_684_800_000_i64;
        while unix_ms < 4_102_444_800_000 {
            let value = unix_ms_to_datetime(unix_ms).unwrap();
            assert_eq!(datetime_to_unix_ms(value), Some(unix_ms));
            unix_ms += MILLIS_PER_DAY;
        }
    }
}
