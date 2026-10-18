//! Shared fixtures for the digest tests.

use super::super::DigestEvent;
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};

/// Far enough in the future that nothing is clipped.
pub const NOW: i64 = 1_893_456_000; // 2030-01-01T00:00:00Z

/// The tick tests' "now": 2026-01-10T06:00:00Z = 11:30 IST, before noon, so
/// an in-progress night is never guessed for "today".
pub fn tick_now() -> i64 {
    rfc3339_to_unix_secs("2026-01-10T06:00:00Z").expect("valid instant")
}

pub fn utc(instant: &str) -> i64 {
    rfc3339_to_unix_secs(instant).expect("valid RFC 3339 instant")
}

/// A self-organized, timed, non-recurring, confirmed event on `zone`.
pub fn event(
    id: &str,
    title: &str,
    start: &str,
    end: &str,
    zone: &str,
    category: Option<&str>,
) -> DigestEvent {
    DigestEvent {
        id: id.to_string(),
        title: title.to_string(),
        start_rfc3339: start.to_string(),
        end_rfc3339: end.to_string(),
        start_time_zone: zone.to_string(),
        end_time_zone: zone.to_string(),
        category_id: category.map(|value| value.to_string()),
        is_all_day: false,
        is_self_organized: true,
        event_type: String::new(),
        recurrence: String::new(),
        status: "confirmed".to_string(),
        deleted: false,
    }
}

/// One 10:00–11:00 UTC event on a UTC clock, with a distinct category so the
/// pipeline tests can read one event's share straight off the day's rows.
pub fn ev(id: &str, start: &str, end: &str, category: Option<&str>) -> DigestEvent {
    event(id, id, start, end, "UTC", category)
}

/// One 10:00–11:00 UTC event per civil date in the inclusive `from..=to` span.
pub fn day_events(from: &str, to: &str) -> Vec<DigestEvent> {
    let mut events = Vec::new();
    let mut date = from.to_string();
    loop {
        events.push(event(
            &format!("ev-{date}"),
            &format!("Work {date}"),
            &format!("{date}T10:00:00Z"),
            &format!("{date}T11:00:00Z"),
            "UTC",
            Some("work"),
        ));
        if date == to {
            break;
        }
        match super::super::shift_date(&date, 1) {
            Some(next) => date = next,
            None => break,
        }
    }
    events
}

/// A 10:00–11:00 local event on `Asia/Kolkata` (a 10:00 CST+5:30 wall clock).
pub fn kolkata_event(id: &str, start: &str, end: &str) -> DigestEvent {
    event(id, id, start, end, "Asia/Kolkata", Some("work"))
}

/// RFC 3339 `Z` rendering of a Unix instant (test convenience).
pub fn z(unix: i64) -> String {
    unix_secs_to_rfc3339(unix)
}
