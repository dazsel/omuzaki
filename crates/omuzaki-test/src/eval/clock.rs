//! Run ids carry the minute they began (`2026-10-13T10:00Z-nightly`): written when a run starts, and read back by `eval gc`
//! to know how old a run folder is. UTC only, no leap seconds, no dependency: it is a civil-calendar conversion.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: i64 = 86_400;

/// The minute `time` falls in, as `YYYY-MM-DDTHH:MMZ`.
#[must_use]
pub fn utc_minute(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let (year, month, day) = civil_from_days(secs.div_euclid(SECS_PER_DAY));
    let in_day = secs.rem_euclid(SECS_PER_DAY);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}Z", in_day / 3600, in_day % 3600 / 60)
}

/// The minute `time` falls in, as a run id begins: `YYYY-MM-DDTHHMMZ`, with no colon, which Windows does not allow in a
/// folder name.
#[must_use]
pub fn run_stamp(time: SystemTime) -> String {
    utc_minute(time).replace(':', "")
}

/// The time a run id begins with (`2026-10-13T1000Z-nightly` is 2026-10-13 10:00 UTC), if it begins with one. The older
/// form with a colon (`2026-10-13T10:00Z-nightly`) is read too.
#[must_use]
pub fn run_started(run_id: &str) -> Option<SystemTime> {
    let (stamp, hour_end, minute_from, minute_to, z) = match run_id.as_bytes().get(13) {
        Some(b':') => (run_id.get(..17)?, 13, 14, 16, 16),
        _ => (run_id.get(..16)?, 13, 13, 15, 15),
    };
    let b = stamp.as_bytes();
    let digits = |from: usize, to: usize| -> Option<i64> {
        let part = stamp.get(from..to)?;
        part.bytes().all(|c| c.is_ascii_digit()).then(|| part.parse().ok()).flatten()
    };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[z] != b'Z' {
        return None;
    }
    let (year, month, day, hour, minute) =
        (digits(0, 4)?, digits(5, 7)?, digits(8, 10)?, digits(11, hour_end)?, digits(minute_from, minute_to)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let secs = days_from_civil(year, month, day) * SECS_PER_DAY + hour * 3600 + minute * 60;
    UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(secs).ok()?))
}

/// The civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// The day count since 1970-01-01 of a civil date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn known_instants_are_written_as_utc_minutes() {
        assert_eq!(utc_minute(at(0)), "1970-01-01T00:00Z");
        assert_eq!(utc_minute(at(951_868_800)), "2000-03-01T00:00Z");
        assert_eq!(utc_minute(at(951_868_800 + 86_399)), "2000-03-01T23:59Z");
        assert_eq!(utc_minute(at(1_709_164_800)), "2024-02-29T00:00Z");
    }

    #[test]
    fn a_run_id_gives_back_the_time_it_began_with() {
        for secs in [0_u64, 951_868_800 + 3600 * 5 + 60 * 7, 1_709_164_800, 1_792_000_000] {
            for stamp in [utc_minute(at(secs)), run_stamp(at(secs))] {
                let id = format!("{stamp}-nightly");
                assert_eq!(run_started(&id), Some(at(secs - secs % 60)), "{id}");
            }
        }
    }

    #[test]
    fn an_id_that_does_not_begin_with_a_time_has_none() {
        for bad in [
            "",
            "manual-1",
            "2026-13-01T00:00Z-x",
            "2026-10-13T25:00Z",
            "2026-10-13T2500Z",
            "2026-10-13T10",
            "2026-10-13 10:00Z",
            "2026-1O-13T10:00Z",
        ] {
            assert_eq!(run_started(bad), None, "{bad}");
        }
    }
}
