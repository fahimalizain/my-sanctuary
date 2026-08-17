//! Timestamp helpers.
//!
//! All persisted timestamps are RFC 3339 UTC strings (D1 `TEXT` columns), and
//! they are *always* derived from a caller-supplied Unix timestamp — never from
//! `SystemTime`, which is unreliable on `wasm32-unknown-unknown`. The Worker
//! sources "now" from `worker::Date::now()` (JS `Date.now()`).

/// Formats a Unix timestamp (seconds) as an RFC 3339 UTC string in the exact
/// shape Go's `time.Now().UTC().Format(time.RFC3339)` produced, e.g.
/// `2026-08-17T12:34:56Z`. UTC strings of this shape sort lexicographically.
pub fn unix_secs_to_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts a count of days since the Unix epoch to a `(year, month, day)`
/// civil date using Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970() {
        assert_eq!(unix_secs_to_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn one_second_before_epoch() {
        assert_eq!(unix_secs_to_rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn known_timestamps() {
        assert_eq!(unix_secs_to_rfc3339(1_234_567_890), "2009-02-13T23:31:30Z");
        assert_eq!(unix_secs_to_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn leap_year_and_month_boundaries() {
        // 2016-02-29 12:00:00Z (2016 is a leap year).
        assert_eq!(unix_secs_to_rfc3339(1_456_747_200), "2016-02-29T12:00:00Z");
        // 2024-12-31 23:59:59Z.
        assert_eq!(unix_secs_to_rfc3339(1_735_689_599), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn google_token_lifetime_lands_in_the_future() {
        let now = 1_700_000_000;
        assert_eq!(
            unix_secs_to_rfc3339(now + 3599),
            "2023-11-14T23:13:19Z"
        );
    }
}
