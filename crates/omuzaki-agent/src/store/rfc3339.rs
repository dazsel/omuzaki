//! RFC 3339 UTC timestamps (`2026-10-14T10:00:00Z`) from and to whole seconds since the Unix epoch.
//!
//! The store writes these into every log line and compares them for retention. A small civil-calendar conversion of
//! our own keeps a date library out of the crate for the one format we need.

/// Seconds in a day.
pub(super) const DAY: i64 = 86_400;

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// (year, month, day) to days since 1970-01-01 (Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * shifted + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub(super) fn format(secs: i64) -> String {
    let (year, month, day) = civil_from_days(secs.div_euclid(DAY));
    let rest = secs.rem_euclid(DAY);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3_600, rest % 3_600 / 60, rest % 60)
}

/// The two-digit-or-longer number at `s[range]`, if it is all digits.
fn digits(s: &str, range: std::ops::Range<usize>) -> Option<i64> {
    let part = s.get(range)?;
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// Seconds since the Unix epoch of a `YYYY-MM-DDTHH:MM:SSZ` string, or `None` if it is not exactly that shape.
pub(super) fn parse(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let shape_ok = b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z';
    if !shape_ok {
        return None;
    }
    let (year, month, day) = (digits(text, 0..4)?, digits(text, 5..7)?, digits(text, 8..10)?);
    let (hour, minute, second) = (digits(text, 11..13)?, digits(text, 14..16)?, digits(text, 17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    // Reject days the month does not have (31 February) by round-tripping.
    if civil_from_days(days) != (year, month, day) {
        return None;
    }
    Some(days * DAY + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_and_a_known_date() {
        assert_eq!(format(0), "1970-01-01T00:00:00Z");
        assert_eq!(format(1_791_972_000), "2026-10-14T10:00:00Z");
        assert_eq!(parse("2026-10-14T10:00:00Z"), Some(1_791_972_000));
    }

    #[test]
    fn leap_days_exist_in_leap_years_only() {
        // 2024 is a leap year, 2100 is not, 2000 is.
        assert_eq!(format(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format(1_709_164_800 + DAY), "2024-03-01T00:00:00Z");
        assert_eq!(parse("2100-02-29T00:00:00Z"), None);
        assert_eq!(format(parse("2100-02-28T23:59:59Z").unwrap() + 1), "2100-03-01T00:00:00Z");
        assert_eq!(format(parse("2000-02-29T12:00:00Z").unwrap()), "2000-02-29T12:00:00Z");
    }

    #[test]
    fn every_day_of_several_years_round_trips() {
        let start = parse("1999-12-31T23:59:59Z").unwrap();
        for step in 0..(366 * 10) {
            let secs = start + step * DAY + step;
            assert_eq!(parse(&format(secs)), Some(secs));
        }
    }

    #[test]
    fn times_before_the_epoch_format() {
        assert_eq!(format(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn malformed_text_is_refused() {
        for bad in [
            "",
            "2026-10-14",
            "2026-10-14T10:00:00",
            "2026-13-01T00:00:00Z",
            "2026-10-14T24:00:00Z",
            "2026-1a-14T10:00:00Z",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
