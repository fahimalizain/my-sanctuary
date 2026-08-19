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

/// Snaps a Unix timestamp (seconds) to the nearest whole minute (half-up):
/// seconds < 30 floor to the current minute, seconds >= 30 ceil to the next.
///
/// Every timer Google write (start/stop/pause/complete/discard/displace)
/// lands on this minute grid so elapsed-time reports never show sub-minute
/// blocks. The result is always `rem_euclid(60) == 0`.
pub fn nearest_minute_unix(secs: i64) -> i64 {
    let rem = secs.rem_euclid(60);
    if rem < 30 {
        secs - rem
    } else {
        secs - rem + 60
    }
}

/// The elongate cron's target: the instant `now_unix + 5 minutes` (the slack)
/// ceiled up onto the 5-minute grid (multiples of 300) **as seen by the
/// event calendar's IANA time zone**.
///
/// The zone is resolved to a fixed UTC offset via [`resolve_tz_offset`] — no
/// TZ database (api-core stays native-testable and small on wasm). The
/// conversion is `local = instant + offset`, ceil the local wall clock,
/// `instant' = ceiled_local - offset`.
///
/// Note: every common civil offset is a whole number of minutes and almost
/// always a multiple of 5 minutes (e.g. `Asia/Kolkata`'s `+05:30` is
/// `19_800 = 66 * 300`), so for those zones the result is numerically
/// identical to a plain UTC ceil — the two grids are the same set of
/// instants. The zone plumbing still matters for spec fidelity ("use the
/// calendar TZ") and for any future non-multiple offset.
pub fn ceil_5min_unix_in_zone(now_unix: i64, iana_tz: &str) -> i64 {
    let offset = resolve_tz_offset(iana_tz);
    let local = now_unix + 300 + offset;
    let rem = local.rem_euclid(300);
    let ceiled_local = if rem == 0 { local } else { local + 300 - rem };
    ceiled_local - offset
}

/// Resolves an IANA time zone to a fixed UTC offset in seconds.
///
/// Locked table (empty/unknown zones fall back to UTC — offset 0):
/// - the UTC family (`UTC`, `Etc/UTC`, `Etc/GMT`, `Z`, `GMT`) and the empty
///   string → 0
/// - `Asia/Kolkata` / `Asia/Calcutta` → `+19800` (the production calendar)
/// - `Asia/Dubai` → `+14400` (cheap fixed-offset addition)
/// - anything else → 0 (UTC fallback, locked)
///
/// No DST-aware zones are modeled; a zone not listed simply behaves as UTC.
fn resolve_tz_offset(iana_tz: &str) -> i64 {
    match iana_tz {
        "" | "UTC" | "Etc/UTC" | "Etc/GMT" | "Z" | "GMT" => 0,
        "Asia/Kolkata" | "Asia/Calcutta" => 19_800, // +05:30 (production)
        "Asia/Dubai" => 14_400,                     // +04:00
        _ => 0,
    }
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

/// Converts a `(year, month, day)` civil date to days since the Unix epoch
/// (inverse of [`civil_from_days`]). Out-of-range days roll into the next
/// month(s), matching Go's `time.Date` normalization.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = m as i64 - 3; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Days in a civil month (leap-aware).
fn days_in_month(year: i64, month: u32) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Parses an RFC 3339 instant (with optional fractional seconds and any
/// numeric offset, e.g. `2026-08-17T12:34:56.789+05:30`) into Unix seconds.
///
/// Fractions are truncated (Go's `time.Unix()` truncates too). Returns `None`
/// for anything that is not RFC 3339. Never touches `SystemTime`.
pub fn rfc3339_to_unix_secs(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut rest = &s[19..];
    // Optional fractional seconds: `.digits…` (truncated, like Go).
    if rest.starts_with('.') {
        let digits_end = rest
            .find(|c: char| c != '.' && !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest = &rest[digits_end..];
    }
    let offset_secs = if rest == "Z" || rest == "z" {
        0
    } else if (rest.starts_with('+') || rest.starts_with('-'))
        && rest.len() == 6
        && rest.as_bytes()[3] == b':'
    {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let offset_hour: i64 = rest.get(1..3)?.parse().ok()?;
        let offset_min: i64 = rest.get(4..6)?.parse().ok()?;
        if offset_hour > 23 || offset_min > 59 {
            return None;
        }
        sign * (offset_hour * 3600 + offset_min * 60)
    } else {
        return None;
    };

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs)
}

/// Adds `months` calendar months to a Unix instant, keeping the wall-clock
/// time-of-day. Day-of-month overflow rolls into the next month(s) exactly
/// like Go's `time.Time.AddDate(0, months, 0)` (e.g. Jan 31 + 1 month is
/// Mar 3). Used for the default event window (`now−1 month … now+2 months`).
pub(crate) fn add_months_unix(secs: i64, months: i64) -> i64 {
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    let total = month as i64 - 1 + months;
    let mut new_year = year + total.div_euclid(12);
    let mut new_month = total.rem_euclid(12) + 1; // 1..=12
    let mut new_day = day as i64;
    // Roll overflowing days into the following month(s), like Go's AddDate.
    loop {
        let dim = days_in_month(new_year, new_month as u32);
        if new_day <= dim {
            break;
        }
        new_day -= dim;
        new_month += 1;
        if new_month > 12 {
            new_month = 1;
            new_year += 1;
        }
    }
    days_from_civil(new_year, new_month as u32, new_day as u32) * 86_400 + time_of_day
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

    #[test]
    fn nearest_minute_floors_below_30_seconds() {
        // 22:13:20 → 22:13:00 (floor).
        assert_eq!(nearest_minute_unix(1_700_000_000), 1_700_000_000 - 20);
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(1_700_000_000)),
            "2023-11-14T22:13:00Z"
        );
        // :00 stays put; :29 floors; :01 floors.
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(rfc3339_to_unix_secs("2023-11-14T22:13:00Z").unwrap())),
            "2023-11-14T22:13:00Z"
        );
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(rfc3339_to_unix_secs("2023-11-14T22:13:29Z").unwrap())),
            "2023-11-14T22:13:00Z"
        );
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(rfc3339_to_unix_secs("2023-11-14T22:13:01Z").unwrap())),
            "2023-11-14T22:13:00Z"
        );
    }

    #[test]
    fn nearest_minute_ceils_at_30_seconds() {
        // 22:13:30 → 22:14:00 (ceil).
        let half_past = rfc3339_to_unix_secs("2023-11-14T22:13:30Z").unwrap();
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(half_past)),
            "2023-11-14T22:14:00Z"
        );
        // :59 also ceils to the next minute.
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(half_past + 29)),
            "2023-11-14T22:14:00Z"
        );
        // Rolls the hour/minute boundary.
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(
                rfc3339_to_unix_secs("2023-11-14T22:59:30Z").unwrap()
            )),
            "2023-11-14T23:00:00Z"
        );
    }

    #[test]
    fn nearest_minute_handles_epoch_and_negatives() {
        // Epoch is already on the grid. Negative instants use `rem_euclid`,
        // so 23:59:30 (unix -30) and 23:59:59 (unix -1) are second 30/59 of
        // the previous minute and ceil to the epoch, not toward -infinity.
        assert_eq!(nearest_minute_unix(0), 0);
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(0)),
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(nearest_minute_unix(-30), 0, "23:59:30 ceils to the epoch");
        assert_eq!(nearest_minute_unix(-1), 0, "23:59:59 ceils to the epoch");
        assert_eq!(nearest_minute_unix(-119), -120, "23:58:01 floors to 23:58:00");
        assert_eq!(nearest_minute_unix(-120), -120, "already on the grid");
        assert_eq!(
            unix_secs_to_rfc3339(nearest_minute_unix(-119)),
            "1969-12-31T23:58:00Z"
        );
        assert_eq!(nearest_minute_unix(30), 60);
    }

    #[test]
    fn ceil_5min_adds_slack_and_ceils_in_utc() {
        // 2026-08-19T11:12:55Z + 5 min = 11:17:55Z → ceil 11:20:00Z.
        let now = rfc3339_to_unix_secs("2026-08-19T11:12:55Z").unwrap();
        assert_eq!(
            unix_secs_to_rfc3339(ceil_5min_unix_in_zone(now, "UTC")),
            "2026-08-19T11:20:00Z"
        );
    }

    #[test]
    fn ceil_5min_stays_put_when_slack_lands_on_grid() {
        // 11:15:00Z + 5 min = 11:20:00Z — already on the 5-min grid.
        let now = rfc3339_to_unix_secs("2026-08-19T11:15:00Z").unwrap();
        assert_eq!(
            unix_secs_to_rfc3339(ceil_5min_unix_in_zone(now, "UTC")),
            "2026-08-19T11:20:00Z"
        );
    }

    #[test]
    fn ceil_5min_rolls_across_midnight() {
        // 23:58:40Z + 5 min = 00:03:40Z → ceil 00:05:00Z next day.
        let now = rfc3339_to_unix_secs("2026-08-19T23:58:40Z").unwrap();
        assert_eq!(
            unix_secs_to_rfc3339(ceil_5min_unix_in_zone(now, "UTC")),
            "2026-08-20T00:05:00Z"
        );
    }

    #[test]
    fn ceil_5min_matches_utc_for_kolkata_since_offset_is_a_5min_multiple() {
        // Asia/Kolkata is +05:30 = 19_800s = 66 * 300, so the 5-minute UTC
        // grid and the IST 5-minute grid are the SAME set of instants — the
        // unix result must match UTC exactly even though the local wall clock
        // reads 16:50 there. (No test can make a multiple-of-5-min offset
        // diverge; the zone path is exercised for spec fidelity.)
        let now = rfc3339_to_unix_secs("2026-08-19T11:12:55Z").unwrap();
        let utc = ceil_5min_unix_in_zone(now, "UTC");
        let ist = ceil_5min_unix_in_zone(now, "Asia/Kolkata");
        assert_eq!(ist, utc, "Kolkata offset is a 5-min multiple");
        assert_eq!(
            unix_secs_to_rfc3339(ist),
            "2026-08-19T11:20:00Z",
            "and IST local of that instant is 16:50:00"
        );
        // The stored instant viewed in IST: 16:47:55 + 5 min → 16:50:00.
        assert_eq!(unix_secs_to_rfc3339(ist + 19_800), "2026-08-19T16:50:00Z");
    }

    #[test]
    fn ceil_5min_empty_and_unknown_zones_fallback_to_utc() {
        let now = rfc3339_to_unix_secs("2026-08-19T11:12:55Z").unwrap();
        let utc = ceil_5min_unix_in_zone(now, "UTC");
        assert_eq!(ceil_5min_unix_in_zone(now, ""), utc, "empty → UTC");
        assert_eq!(ceil_5min_unix_in_zone(now, "Etc/UTC"), utc);
        assert_eq!(ceil_5min_unix_in_zone(now, "Etc/GMT"), utc);
        assert_eq!(ceil_5min_unix_in_zone(now, "America/New_York"), utc, "unknown → UTC");
        assert_eq!(ceil_5min_unix_in_zone(now, "bogus"), utc);
    }

    #[test]
    fn tz_offset_resolver_is_fixed_offsets_or_utc_fallback() {
        for utc in ["", "UTC", "Etc/UTC", "Etc/GMT", "Z", "GMT"] {
            assert_eq!(resolve_tz_offset(utc), 0, "{utc:?} → 0");
        }
        assert_eq!(resolve_tz_offset("Asia/Kolkata"), 19_800);
        assert_eq!(resolve_tz_offset("Asia/Calcutta"), 19_800);
        assert_eq!(resolve_tz_offset("Asia/Dubai"), 14_400);
        assert_eq!(resolve_tz_offset("America/New_York"), 0, "unmodeled zone → UTC fallback");
    }

    #[test]
    fn parses_rfc3339_with_zulu_offset() {
        assert_eq!(rfc3339_to_unix_secs("2023-11-14T22:13:20Z"), Some(1_700_000_000));
        assert_eq!(rfc3339_to_unix_secs("2016-02-29T12:00:00Z"), Some(1_456_747_200));
    }

    #[test]
    fn parses_fractional_seconds_truncated() {
        // Same instant as the epoch, half a second in — truncates like Go.
        assert_eq!(rfc3339_to_unix_secs("1970-01-01T00:00:00.999Z"), Some(0));
        assert_eq!(
            rfc3339_to_unix_secs("2023-11-14T22:13:20.789123Z"),
            Some(1_700_000_000)
        );
    }

    #[test]
    fn parses_numeric_offsets_into_utc() {
        // 22:13:20 +05:30 == 16:43:20Z (offset 19_800s subtracted).
        assert_eq!(rfc3339_to_unix_secs("2023-11-14T22:13:20+05:30"), Some(1_699_980_200));
        // 22:13:20 -02:00 == 2023-11-15T00:13:20Z (offset added back).
        assert_eq!(rfc3339_to_unix_secs("2023-11-14T22:13:20-02:00"), Some(1_700_007_200));
    }

    #[test]
    fn roundtrip_rfc3339_and_unix() {
        for secs in [0, 1_456_747_200, 1_700_000_000, 1_735_689_599, -1, 1_700_000_000 + 3599] {
            let formatted = unix_secs_to_rfc3339(secs);
            assert_eq!(rfc3339_to_unix_secs(&formatted), Some(secs), "{formatted}");
        }
    }

    #[test]
    fn rejects_malformed_rfc3339() {
        for bad in [
            "not a date",
            "2023-11-14",
            "2023-11-14T22:13:20",      // missing offset
            "2023-11-14T22:13:20X",     // garbage offset
            "2023-13-01T00:00:00Z",     // month 13
            "2023-11-14T25:00:00Z",     // hour 25
            "2023-11-14T22:13:61Z",     // second 61
            "2023-11-14T22:13:20+25:00", // offset hour 25
            "2023-11-14 22:13:20Z",     // space instead of T
            "",                          // empty
        ] {
            assert_eq!(rfc3339_to_unix_secs(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn add_months_keeps_time_of_day() {
        // 2023-11-14T22:13:20Z minus 1 month.
        assert_eq!(
            unix_secs_to_rfc3339(add_months_unix(1_700_000_000, -1)),
            "2023-10-14T22:13:20Z"
        );
        // Plus 2 months.
        assert_eq!(
            unix_secs_to_rfc3339(add_months_unix(1_700_000_000, 2)),
            "2024-01-14T22:13:20Z"
        );
    }

    #[test]
    fn add_months_normalizes_day_overflow_like_go() {
        // Jan 31 + 1 month → Mar 3 (Feb has 28 days in 2026).
        let jan31 = rfc3339_to_unix_secs("2026-01-31T12:00:00Z").unwrap();
        assert_eq!(
            unix_secs_to_rfc3339(add_months_unix(jan31, 1)),
            "2026-03-03T12:00:00Z"
        );
        // Leap year: Jan 31 + 1 month → Mar 2 (2024 is a leap year).
        let leap_jan31 = rfc3339_to_unix_secs("2024-01-31T12:00:00Z").unwrap();
        assert_eq!(
            unix_secs_to_rfc3339(add_months_unix(leap_jan31, 1)),
            "2024-03-02T12:00:00Z"
        );
        // Dec + 2 months rolls the year.
        let dec15 = rfc3339_to_unix_secs("2026-12-15T08:00:00Z").unwrap();
        assert_eq!(
            unix_secs_to_rfc3339(add_months_unix(dec15, 2)),
            "2027-02-15T08:00:00Z"
        );
    }
}
