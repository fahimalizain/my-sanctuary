//! Digest pipeline tests: overlap sharing, filters, dating, day zones, sleep.
//!
//! Zones are `UTC` unless a test is explicitly about IANA zones; instants are
//! built as RFC 3339 `Z` strings through `unix_secs_to_rfc3339`.

use std::collections::BTreeMap;

use super::repo::{dirty_dates_for_span, EventSpan};
use super::sleep::{marker_kind, MarkerKind};
use super::*;

pub mod support;
pub mod tick;

pub use support::{event, ev, utc, NOW};

fn find<'a>(days: &'a [DayDigest], date: &str) -> &'a DayDigest {
    days.iter()
        .find(|day| day.local_date == date)
        .unwrap_or_else(|| {
            panic!(
                "no digest for {date}; got {:?}",
                days.iter().map(|day| &day.local_date).collect::<Vec<_>>()
            )
        })
}

fn categories(day: &DayDigest) -> BTreeMap<String, i64> {
    day.category_seconds
        .iter()
        .map(|row| (row.category_id.clone(), row.seconds))
        .collect()
}

fn hours(day: &DayDigest) -> BTreeMap<(i64, String), i64> {
    day.hour_seconds
        .iter()
        .map(|row| ((row.hour, row.category_id.clone()), row.seconds))
        .collect()
}

fn all_nights(days: &[DayDigest]) -> Vec<NightRow> {
    days.iter()
        .flat_map(|day| day.nights.iter().cloned())
        .collect()
}

/// Every emitted date satisfies `sum(categories) == tracked == sum(hours)`.
fn assert_invariants(days: &[DayDigest]) {
    for day in days {
        let category_total: i64 = day.category_seconds.iter().map(|row| row.seconds).sum();
        let hour_total: i64 = day.hour_seconds.iter().map(|row| row.seconds).sum();
        assert_eq!(
            category_total, day.tracked_seconds,
            "category rows must sum to tracked on {}",
            day.local_date
        );
        assert_eq!(
            hour_total, day.tracked_seconds,
            "hour rows must sum to tracked on {}",
            day.local_date
        );
        for row in &day.hour_seconds {
            assert!(
                (0..24).contains(&row.hour),
                "hour {} out of range on {}",
                row.hour,
                day.local_date
            );
        }
    }
}

// ──────────────────────────────────────────
// Overlap sharing
// ──────────────────────────────────────────

#[test]
fn share_matches_the_readme_example() {
    // Parent 10:00–11:00 with three touching children inside it.
    let events = vec![
        ev("p", "2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z", Some("p")),
        ev("a", "2026-01-01T10:00:00Z", "2026-01-01T10:20:00Z", Some("a")),
        ev("b", "2026-01-01T10:20:00Z", "2026-01-01T10:40:00Z", Some("b")),
        ev("c", "2026-01-01T10:40:00Z", "2026-01-01T11:00:00Z", Some("c")),
    ];
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&days, "2026-01-01");
    let by_category = categories(day);
    assert_eq!(by_category.get("p"), Some(&1800), "parent keeps 30 minutes");
    assert_eq!(by_category.get("a"), Some(&600));
    assert_eq!(by_category.get("b"), Some(&600));
    assert_eq!(by_category.get("c"), Some(&600));
    assert_eq!(day.tracked_seconds, 3600, "the union of the intervals");
    assert_invariants(&days);
}

#[test]
fn share_splits_three_fully_overlapping_events() {
    let events = vec![
        ev("x", "2026-01-01T10:00:00Z", "2026-01-01T10:10:00Z", Some("x")),
        ev("y", "2026-01-01T10:00:00Z", "2026-01-01T10:10:00Z", Some("y")),
        ev("z", "2026-01-01T10:00:00Z", "2026-01-01T10:10:00Z", Some("z")),
    ];
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&days, "2026-01-01");
    let by_category = categories(day);
    assert_eq!(by_category.get("x"), Some(&200));
    assert_eq!(by_category.get("y"), Some(&200));
    assert_eq!(by_category.get("z"), Some(&200));
    assert_invariants(&days);
}

#[test]
fn share_gives_the_remainder_to_the_smallest_id() {
    // Three identical 10-second events: base 3 each, one leftover second to
    // the smallest id (`a`).
    let events = vec![
        ev("a", "2026-01-01T10:00:00Z", "2026-01-01T10:00:10Z", Some("a")),
        ev("b", "2026-01-01T10:00:00Z", "2026-01-01T10:00:10Z", Some("b")),
        ev("c", "2026-01-01T10:00:00Z", "2026-01-01T10:00:10Z", Some("c")),
    ];
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&days, "2026-01-01");
    let by_category = categories(day);
    assert_eq!(by_category.get("a"), Some(&4));
    assert_eq!(by_category.get("b"), Some(&3));
    assert_eq!(by_category.get("c"), Some(&3));
    assert_eq!(day.tracked_seconds, 10);
    assert_invariants(&days);
}

#[test]
fn share_boundaries_use_the_python_active_predicate() {
    // Boundaries are the union of every start and end; a segment is active
    // when it covers the slice (`start <= slice_start && end >= slice_end`).
    let segments = vec![
        Segment {
            key: SegKey {
                id: "a".to_string(),
                ord: 0,
            },
            local_date: "2026-01-01".to_string(),
            start: 0,
            end: 100,
            zone: "UTC".to_string(),
            category_id: "a".to_string(),
            synthetic: false,
            is_marker: false,
        },
        Segment {
            key: SegKey {
                id: "b".to_string(),
                ord: 1,
            },
            local_date: "2026-01-01".to_string(),
            start: 50,
            end: 150,
            zone: "UTC".to_string(),
            category_id: "b".to_string(),
            synthetic: false,
            is_marker: false,
        },
    ];
    // [0,50) only `a`; [50,100) both; [100,150) only `b`.
    assert_eq!(share_seconds(&segments), vec![75, 75]);
    // The parts always sum to the union of the intervals.
    let total: i64 = share_seconds(&segments).iter().sum();
    assert_eq!(total, 150);
}

// ──────────────────────────────────────────
// Dating, splitting, clipping
// ──────────────────────────────────────────

#[test]
fn overnight_event_splits_on_local_midnights_in_the_start_zone() {
    // 23:30 → 00:30 IST: one hour on each civil date.
    let events = vec![event(
        "night",
        "night",
        "2026-01-01T23:30:00+05:30",
        "2026-01-02T00:30:00+05:30",
        "Asia/Kolkata",
        Some("night"),
    )];
    let days = digest_days(&events, &[], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-01").tracked_seconds, 1800);
    assert_eq!(find(&days, "2026-01-02").tracked_seconds, 1800);
    assert_eq!(categories(find(&days, "2026-01-01")).get("night"), Some(&1800));
    assert_eq!(categories(find(&days, "2026-01-02")).get("night"), Some(&1800));
    assert_invariants(&days);
}

#[test]
fn event_ending_exactly_on_local_midnight_is_not_split() {
    // `[start, end)` — the tail at exactly 00:00 is empty, so there is no
    // second date and no zero-length segment.
    let events = vec![event(
        "evening",
        "evening",
        "2026-01-01T22:00:00+05:30",
        "2026-01-02T00:00:00+05:30",
        "Asia/Kolkata",
        Some("evening"),
    )];
    let days = digest_days(&events, &[], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(days.len(), 1, "one civil date only: {days:?}");
    assert_eq!(days[0].local_date, "2026-01-01");
    assert_eq!(days[0].tracked_seconds, 7200);
    assert_invariants(&days);
}

#[test]
fn a_shorter_later_zone_does_not_move_the_split() {
    // The whole event is dated and split in the START zone; the end zone is
    // only a fallback when the start zone is empty.
    let events = vec![event(
        "travel",
        "travel",
        "2026-01-01T23:00:00+05:30",
        "2026-01-02T01:00:00+05:30",
        "Asia/Kolkata",
        Some("travel"),
    )];
    let mut events = events;
    events[0].end_time_zone = "Europe/London".to_string();
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-01").tracked_seconds, 3600);
    assert_eq!(find(&days, "2026-01-02").tracked_seconds, 3600);
    assert_eq!(
        find(&days, "2026-01-01").time_zone,
        "Asia/Kolkata",
        "the vote follows the start zone"
    );
}

#[test]
fn empty_start_zone_falls_back_to_the_end_zone() {
    let mut event = event(
        "no-start-zone",
        "no-start-zone",
        "2026-01-01T18:30:00Z",
        "2026-01-01T19:30:00Z",
        "",
        Some("no-start-zone"),
    );
    event.end_time_zone = "Asia/Kolkata".to_string();
    let days = digest_days(&[event], &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    // 18:30Z is 00:00 IST on the 2nd, so the date comes from the end zone.
    assert_eq!(find(&days, "2026-01-02").tracked_seconds, 3600);
    assert_eq!(find(&days, "2026-01-02").time_zone, "Asia/Kolkata");
}

#[test]
fn clip_to_now_cuts_the_running_interval() {
    // 10:00–12:00 with now at 11:00 → 3600s (today is provisional).
    let now = utc("2026-01-01T11:00:00Z");
    let events = vec![ev("run", "2026-01-01T10:00:00Z", "2026-01-01T12:00:00Z", Some("run"))];
    let days = digest_days(&events, &["2026-01-01".to_string()], now, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(categories(find(&days, "2026-01-01")).get("run"), Some(&3600));
    assert_invariants(&days);
}

#[test]
fn clip_drops_an_event_that_starts_exactly_at_now() {
    let now = utc("2026-01-01T11:00:00Z");
    let events = vec![ev("future", "2026-01-01T11:00:00Z", "2026-01-01T12:00:00Z", Some("future"))];
    let days = digest_days(&events, &["2026-01-01".to_string()], now, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&days, "2026-01-01");
    assert_eq!(categories(day).get("future"), None);
    assert_eq!(day.tracked_seconds, 0, "nothing but the estimated night");
}

#[test]
fn unparseable_instants_drop_the_event() {
    let mut bad = ev("bad", "2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z", Some("bad"));
    bad.start_rfc3339 = "not a date".to_string();
    // No explicit dates ⇒ no sleep estimation, so the result is only about the
    // surviving segments.
    let days = digest_days(&[bad], &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert!(days.is_empty(), "the only event was dropped: {days:?}");
}

// ──────────────────────────────────────────
// Filters
// ──────────────────────────────────────────

/// Asserts a mutated event never reaches the pool: with no explicit dates
/// there is no sleep estimation either, so the result is empty.
fn filtered_out(label: &str, mutate: impl FnOnce(&mut DigestEvent)) {
    let mut event = ev("row", "2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z", Some("row"));
    mutate(&mut event);
    let days = digest_days(&[event], &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert!(days.is_empty(), "{label} must be dropped: {days:?}");
}

fn kept(label: &str, mutate: impl FnOnce(&mut DigestEvent)) {
    let mut event = ev("row", "2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z", Some("row"));
    mutate(&mut event);
    let days = digest_days(&[event], &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(
        categories(find(&days, "2026-01-01")).get("row"),
        Some(&3600),
        "{label} must be kept"
    );
}

#[test]
fn filters_drop_deleted_cancelled_masters_all_day_and_foreign_rows() {
    filtered_out("deleted", |event| event.deleted = true);
    filtered_out("cancelled", |event| {
        event.status = "cancelled".to_string()
    });
    filtered_out("recurring master", |event| {
        event.recurrence = "RRULE:FREQ=DAILY".to_string()
    });
    filtered_out("rec def", |event| {
        event.recurrence = "EXDATE:20260101T100000Z".to_string()
    });
    filtered_out("all-day", |event| event.is_all_day = true);
    filtered_out("non-self-organized", |event| {
        event.is_self_organized = false
    });
    filtered_out("focusTime", |event| {
        event.event_type = "focusTime".to_string()
    });
    filtered_out("workingLocation", |event| {
        event.event_type = "workingLocation".to_string()
    });
    filtered_out("outOfOffice", |event| {
        event.event_type = "outOfOffice".to_string()
    });
    filtered_out("birthday", |event| {
        event.event_type = "birthday".to_string()
    });
}

#[test]
fn filters_keep_legacy_and_gmail_event_types() {
    kept("legacy empty event_type", |_| {});
    kept("default", |event| event.event_type = "default".to_string());
    kept("fromGmail", |event| {
        event.event_type = "fromGmail".to_string()
    });
    // A whitespace-only recurrence is not a master.
    kept("blank recurrence", |event| {
        event.recurrence = "   ".to_string()
    });
    // Only the exact Google value is cancelled.
    kept("tentative", |event| {
        event.status = "tentative".to_string()
    });
    // Legacy rows default to self-organized (recorded as an explicit flag).
    kept("no organizer flag", |event| {
        event.is_self_organized = true
    });
}

#[test]
fn a_master_with_a_huge_duration_is_excluded_while_a_normal_row_is_not() {
    // A daily master spanning a year would otherwise flood every day.
    let mut master = ev(
        "master",
        "2026-01-01T00:00:00Z",
        "2027-01-01T00:00:00Z",
        Some("master"),
    );
    master.recurrence = "RRULE:FREQ=DAILY".to_string();
    let normal = ev("normal", "2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z", Some("normal"));
    let days = digest_days(&[master, normal], &["2026-01-01".to_string()], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&days, "2026-01-01");
    assert_eq!(categories(day).get("master"), None);
    assert_eq!(categories(day).get("normal"), Some(&3600));
}

#[test]
fn cancelled_exception_is_excluded_by_status() {
    // A stored cancelled exception (living row, `status = cancelled`).
    let mut exception = ev(
        "ex",
        "2026-01-01T10:00:00Z",
        "2026-01-01T11:00:00Z",
        Some("ex"),
    );
    exception.status = "cancelled".to_string();
    let days = digest_days(&[exception], &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert!(days.is_empty(), "a cancelled exception is dropped");
}

// ──────────────────────────────────────────
// Day time zone
// ──────────────────────────────────────────

#[test]
fn day_zone_takes_the_highest_pre_share_weight() {
    // Both events date themselves onto 2026-01-01 (07:00Z and 17:00Z are the
    // same Kolkata civil date), but they vote for different zones: 10h
    // Kolkata beats 2h London.
    let events = vec![
        event(
            "kolkata",
            "kolkata",
            "2026-01-01T07:00:00Z",
            "2026-01-01T17:00:00Z",
            "Asia/Kolkata",
            Some("kolkata"),
        ),
        event(
            "london",
            "london",
            "2026-01-01T12:00:00Z",
            "2026-01-01T14:00:00Z",
            "Europe/London",
            Some("london"),
        ),
    ];
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-01").time_zone, "Asia/Kolkata");
    assert_invariants(&days);
}

#[test]
fn day_zone_weight_is_the_clipped_interval_not_the_shared_one() {
    // Same 10h/2h vote, but the 2h event is fully inside the 10h one: sharing
    // leaves the long event with 8h of the day and the short one with 2h —
    // the vote must still use the pre-share 10h.
    let events = vec![
        event(
            "kolkata",
            "kolkata",
            "2026-01-01T07:00:00Z",
            "2026-01-01T17:00:00Z",
            "Asia/Kolkata",
            Some("kolkata"),
        ),
        event(
            "london",
            "london",
            "2026-01-01T07:00:00Z",
            "2026-01-01T09:00:00Z",
            "Europe/London",
            Some("london"),
        ),
    ];
    // 07:00Z is 12:30 IST and 07:00 GMT, both on 2026-01-01.
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&days, "2026-01-01");
    assert_eq!(day.time_zone, "Asia/Kolkata");
    let by_category = categories(day);
    // The 10h event keeps 8h around the shared 2h slice, plus its half of it.
    assert_eq!(by_category.get("kolkata"), Some(&(28800 + 3600)));
    assert_eq!(by_category.get("london"), Some(&3600));
    assert_invariants(&days);
}

#[test]
fn day_zone_ties_break_on_the_lexicographically_smaller_zone() {
    let events = vec![
        event(
            "london",
            "london",
            "2026-01-01T12:00:00Z",
            "2026-01-01T14:00:00Z",
            "Europe/London",
            Some("london"),
        ),
        event(
            "kolkata",
            "kolkata",
            "2026-01-01T12:00:00Z",
            "2026-01-01T14:00:00Z",
            "Asia/Kolkata",
            Some("kolkata"),
        ),
    ];
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    // `Asia/Kolkata` < `Europe/London`.
    assert_eq!(find(&days, "2026-01-01").time_zone, "Asia/Kolkata");
}

#[test]
fn day_zone_inherits_the_previous_date_then_the_fallback_then_utc() {
    let events = vec![event(
        "kolkata",
        "kolkata",
        "2026-01-01T04:00:00Z",
        "2026-01-01T14:00:00Z",
        "Asia/Kolkata",
        Some("kolkata"),
    )];
    // 04:00Z–14:00Z is 09:30–19:30 IST on 2026-01-01.
    let dates = ["2026-01-01".to_string(), "2026-01-02".to_string(), "2026-01-03".to_string()];
    let days = digest_days(&events, &dates, NOW, "Europe/London", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-01").time_zone, "Asia/Kolkata");
    assert_eq!(find(&days, "2026-01-02").time_zone, "Asia/Kolkata", "carried forward");
    assert_eq!(find(&days, "2026-01-03").time_zone, "Asia/Kolkata", "carried again");

    // A date before any vote has nothing to inherit.
    let days = digest_days(&events, &["2025-12-31".to_string()], NOW, "Europe/London", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(days[0].time_zone, "Europe/London");

    // An empty/unknown fallback is UTC (ADR 0004).
    let days = digest_days(
        &events,
        &["2025-12-31".to_string()],
        NOW,
        "",
        None,
        None,
        DEFAULT_SLEEP_SECS,
    );
    assert_eq!(days[0].time_zone, "UTC");
    let days = digest_days(
        &events,
        &["2025-12-31".to_string()],
        NOW,
        "bogus/zone",
        None,
        None,
        DEFAULT_SLEEP_SECS,
    );
    assert_eq!(days[0].time_zone, "UTC");
}

#[test]
fn an_empty_day_inherits_the_persisted_zone_not_the_batch() {
    // A one-day rebuild of a day with no events must not fall through to the
    // primary zone (or UTC) when the day before it is persisted as Kolkata:
    // the inferred zone is a function of this batch's votes plus persisted
    // history, never of which other dirty dates happened to share the batch.
    let dates = ["2026-01-02".to_string()];
    let days = digest_days(&[], &dates, NOW, "UTC", Some("Asia/Kolkata"), None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-02").time_zone, "Asia/Kolkata");

    // No persisted history: the same call falls back to the primary zone.
    let days = digest_days(&[], &dates, NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-02").time_zone, "UTC");

    // A vote inside the batch still wins over persisted history.
    let voted = vec![event(
        "kolkata",
        "kolkata",
        "2026-01-02T04:00:00Z",
        "2026-01-02T14:00:00Z",
        "Asia/Kolkata",
        Some("kolkata"),
    )];
    let days = digest_days(&voted, &dates, NOW, "UTC", Some("Europe/London"), None, DEFAULT_SLEEP_SECS);
    assert_eq!(
        find(&days, "2026-01-02").time_zone,
        "Asia/Kolkata",
        "the batch's own vote is stronger than history"
    );

    // Empty and unknown strings are not zones (ADR 0004).
    let days = digest_days(&[], &dates, NOW, "UTC", Some(""), None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-02").time_zone, "UTC");
    let days = digest_days(&[], &dates, NOW, "UTC", Some("bogus/zone"), None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&days, "2026-01-02").time_zone, "UTC");
}

// ──────────────────────────────────────────
// Hours
// ──────────────────────────────────────────

#[test]
fn hours_split_on_the_day_zone_clock() {
    let events = vec![event(
        "morning",
        "morning",
        "2026-01-01T10:30:00Z",
        "2026-01-01T11:30:00Z",
        "UTC",
        Some("morning"),
    )];
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let by_hour = hours(find(&days, "2026-01-01"));
    assert_eq!(by_hour.get(&(10, "morning".to_string())), Some(&1800));
    assert_eq!(by_hour.get(&(11, "morning".to_string())), Some(&1800));
    assert_invariants(&days);
}

#[test]
fn hours_use_the_winning_zone_even_for_events_zoned_elsewhere() {
    // The day's winning zone is Asia/Kolkata, so a London-zoned event's hours
    // are the IST clock hours (17:30–19:30).
    let events = vec![
        event(
            "kolkata",
            "kolkata",
            "2026-01-01T07:00:00Z",
            "2026-01-01T17:00:00Z",
            "Asia/Kolkata",
            Some("kolkata"),
        ),
        event(
            "london",
            "london",
            "2026-01-01T12:00:00Z",
            "2026-01-01T14:00:00Z",
            "Europe/London",
            Some("london"),
        ),
    ];
    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&days, "2026-01-01");
    let by_hour = hours(day);
    // The London event's 3600 shared seconds spread over its IST hour slices
    // (17:30–18:00, 18:00–19:00, 19:00–19:30) proportionally and exactly.
    assert_eq!(by_hour.get(&(17, "london".to_string())), Some(&900));
    assert_eq!(by_hour.get(&(18, "london".to_string())), Some(&1800));
    assert_eq!(by_hour.get(&(19, "london".to_string())), Some(&900));
    // Kolkata keeps its own time around the shared slice, plus its share.
    let kolkata_hours: i64 = day
        .hour_seconds
        .iter()
        .filter(|row| row.category_id == "kolkata")
        .map(|row| row.seconds)
        .sum();
    assert_eq!(kolkata_hours, 32400);
    assert_invariants(&days);
}

// ──────────────────────────────────────────
// Untracked fill
// ──────────────────────────────────────────

#[test]
fn untracked_is_none_for_today_and_a_fill_for_other_days() {
    let now = utc("2026-01-02T10:00:00Z");
    assert_eq!(untracked_seconds(3600, "2026-01-02", "UTC", now), None, "today");
    assert_eq!(untracked_seconds(3600, "2026-01-01", "UTC", now), Some(86_400 - 3600));
    // Kolkata is 5.5h ahead: at 10:00Z it is already the 2nd there too.
    assert_eq!(untracked_seconds(0, "2026-01-01", "Asia/Kolkata", now), Some(86_400));
    // Over-counted days floor at zero instead of going negative.
    assert_eq!(untracked_seconds(90_000, "2026-01-01", "UTC", now), Some(0));
}

// ──────────────────────────────────────────
// Sleep: pairing
// ──────────────────────────────────────────

fn marker(id: &str, title: &str, start: &str, end: &str) -> DigestEvent {
    event(id, title, start, end, "Asia/Kolkata", None)
}

#[test]
fn touching_markers_pair_into_a_real_night() {
    // The Asleep block runs until the Woke Up event starts — the case that
    // `sleep_events.py` (using the Asleep *end*) collapsed to 0 hours.
    let events = vec![
        marker(
            "asleep",
            "Asleep",
            "2026-01-01T22:30:00+05:30",
            "2026-01-02T05:00:00+05:30",
        ),
        marker(
            "woke",
            "Woke Up",
            "2026-01-02T05:00:00+05:30",
            "2026-01-02T05:01:00+05:30",
        ),
    ];
    let days = digest_days(
        &events,
        &["2026-01-02".to_string()],
        NOW,
        "Asia/Kolkata",
        None,
        None,
        DEFAULT_SLEEP_SECS,
    );
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1, "exactly one paired night: {nights:?}");
    let night = &nights[0];
    assert_eq!(night.seconds, 23_400, "6.5h, not zero");
    assert_eq!(night.source, SleepSource::Paired);
    assert_eq!(night.wake_date, "2026-01-02");
    assert_eq!(night.asleep_at, "2026-01-01T17:00:00Z");
    assert_eq!(night.woke_at, "2026-01-01T23:30:00Z");
    assert_eq!(night.bed_min_past_noon, 630, "22:30 → 630");
    assert_eq!(night.wake_min_past_midnight, 300, "05:00 → 300");
    // The synthetic interval shares its slice with the (identical) Asleep
    // event: half of the post-midnight half stays synthetic.
    let day = find(&days, "2026-01-02");
    assert_eq!(day.sleep_seconds, 9_000);
    assert_eq!(day.tracked_seconds, 9_000 + 9_000 + 60);
    assert_invariants(&days);
}

#[test]
fn a_short_evening_pair_does_not_steal_the_morning_wake() {
    // Four markers, one night: the 23:30 wake and the 23:35 bedtime are
    // intermediates of 23:00 → 06:00. The old oldest-asleep-first walk paired
    // 23:00 with 23:30 (0.5h) and then 23:35 with 06:00 (~6.4h).
    let events = vec![
        marker("asleep-1", "Asleep", "2026-01-01T23:00:00+05:30", "2026-01-01T23:01:00+05:30"),
        marker("woke-1", "Woke Up", "2026-01-01T23:30:00+05:30", "2026-01-01T23:31:00+05:30"),
        marker("asleep-2", "Asleep", "2026-01-01T23:35:00+05:30", "2026-01-01T23:36:00+05:30"),
        marker("woke-2", "Woke Up", "2026-01-02T06:00:00+05:30", "2026-01-02T06:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(
        nights.len(),
        1,
        "one night, not an evening pair plus a morning: {nights:?}"
    );
    let night = &nights[0];
    assert_eq!(night.seconds, 25_200, "23:00 → 06:00 is 7h");
    assert_eq!(night.source, SleepSource::Paired);
    assert_eq!(night.wake_date, "2026-01-02", "the morning's civil date");
    assert_eq!(night.asleep_at, "2026-01-01T17:30:00Z", "23:00 IST");
    assert_eq!(night.woke_at, "2026-01-02T00:30:00Z", "06:00 IST");
    assert!(
        nights
            .iter()
            .all(|night| night.woke_at != "2026-01-01T18:00:00Z"),
        "the 23:30 wake must not pair with the 23:00 bedtime"
    );
}

#[test]
fn a_lone_short_pair_still_counts() {
    // Asleep 22:00, Woke Up 22:30 the same evening, with no later wake to
    // cascade to → a 1800s paired night. The 2-hour floor stays removed.
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:00:00+05:30", "2026-01-01T22:20:00+05:30"),
        marker("woke", "Woke Up", "2026-01-01T22:30:00+05:30", "2026-01-01T22:31:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-01".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].seconds, 1800);
    assert_eq!(nights[0].source, SleepSource::Paired);
    assert_eq!(nights[0].wake_date, "2026-01-01");
    assert_eq!(nights[0].bed_min_past_noon, 600, "22:00 → 600");
    assert_eq!(nights[0].wake_min_past_midnight, 1350, "22:30 → 1350");
}

#[test]
fn a_ninety_minute_night_across_midnight_still_pairs() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T23:30:00+05:30", "2026-01-02T01:00:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T01:00:00+05:30", "2026-01-02T01:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].seconds, 5400, "90 minutes");
    assert_eq!(nights[0].source, SleepSource::Paired);
}

#[test]
fn sixteen_hours_pairs_and_one_second_more_does_not() {
    let exact = vec![
        marker("asleep", "Asleep", "2026-01-01T12:00:00+05:30", "2026-01-01T12:01:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T04:00:00+05:30", "2026-01-02T04:01:00+05:30"),
    ];
    let days = digest_days(&exact, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].seconds, 57_600, "16h exactly");

    let too_long = vec![
        marker("asleep", "Asleep", "2026-01-01T12:00:00+05:30", "2026-01-01T12:01:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T04:00:01+05:30", "2026-01-02T04:01:00+05:30"),
    ];
    let days = digest_days(
        &too_long,
        &["2026-01-02".to_string()],
        NOW,
        "Asia/Kolkata",
        None,
        None,
        DEFAULT_SLEEP_SECS,
    );
    assert!(
        all_nights(&days)
            .iter()
            .all(|night| night.source != SleepSource::Paired),
        "16h + 1s must not pair"
    );
}

#[test]
fn same_instant_asleep_and_woke_do_not_pair() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:00:00+05:30", "2026-01-01T22:01:00+05:30"),
        marker("woke", "Woke Up", "2026-01-01T22:00:00+05:30", "2026-01-01T22:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-01".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert!(
        all_nights(&days)
            .iter()
            .all(|night| night.source != SleepSource::Paired),
        "a zero-length night is not a night"
    );
}

#[test]
fn an_early_morning_asleep_pairs_with_its_morning_wake() {
    // Asleep 01:00 with Woke Up 08:00 is one night on that wake_date; the
    // bedtime is more than 12h past noon.
    let events = vec![
        marker("asleep", "Asleep", "2026-01-02T01:00:00+05:30", "2026-01-02T01:01:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T08:00:00+05:30", "2026-01-02T08:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].seconds, 25_200, "7h");
    assert_eq!(nights[0].wake_date, "2026-01-02");
    assert_eq!(nights[0].bed_min_past_noon, 780, "01:00 → 720 + 60");
    assert_eq!(nights[0].wake_min_past_midnight, 480, "08:00 → 480");
}

#[test]
fn marker_titles_are_case_sensitive_and_ambiguity_is_ignored() {
    assert_eq!(marker_kind("Asleep"), MarkerKind::Asleep);
    assert_eq!(marker_kind("Going Asleep now"), MarkerKind::Asleep);
    assert_eq!(marker_kind("  Woke Up  "), MarkerKind::Woke);
    assert_eq!(marker_kind("asleep"), MarkerKind::NotAMarker);
    assert_eq!(marker_kind("woke up"), MarkerKind::NotAMarker);
    assert_eq!(marker_kind("Lunch"), MarkerKind::NotAMarker);
    assert_eq!(marker_kind("Asleep then Woke Up"), MarkerKind::Ambiguous);

    // A wrong-case title is a real event, not a marker: it never pairs.
    let events = vec![
        marker("asleep", "asleep", "2026-01-01T22:00:00+05:30", "2026-01-01T22:01:00+05:30"),
        marker("woke", "woke up", "2026-01-02T06:00:00+05:30", "2026-01-02T06:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert!(all_nights(&days)
        .iter()
        .all(|night| night.source != SleepSource::Paired));
}

// ──────────────────────────────────────────
// Sleep: estimation
// ──────────────────────────────────────────

#[test]
fn est_no_wake_uses_the_default_duration_when_it_is_earlier() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T23:00:00+05:30", "2026-01-01T23:01:00+05:30"),
        event(
            "first",
            "first",
            "2026-01-02T08:00:00+05:30",
            "2026-01-02T09:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].source, SleepSource::EstNoWake);
    assert_eq!(nights[0].wake_date, "2026-01-02");
    assert_eq!(nights[0].asleep_at, "2026-01-01T17:30:00Z");
    assert_eq!(nights[0].woke_at, "2026-01-02T00:00:00Z", "05:30 IST");
    assert_eq!(nights[0].seconds, 23_400);
}

#[test]
fn est_no_wake_clamps_to_the_first_event() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T23:00:00+05:30", "2026-01-01T23:01:00+05:30"),
        event(
            "first",
            "first",
            "2026-01-02T04:00:00+05:30",
            "2026-01-02T05:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].source, SleepSource::EstNoWake);
    assert_eq!(nights[0].woke_at, "2026-01-01T22:30:00Z", "04:00 IST");
}

#[test]
fn est_no_asleep_starts_six_and_a_half_hours_before_the_wake() {
    let events = vec![
        event(
            "last",
            "last",
            "2026-01-01T22:00:00+05:30",
            "2026-01-01T23:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
        marker("woke", "Woke Up", "2026-01-02T06:00:00+05:30", "2026-01-02T06:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].source, SleepSource::EstNoAsleep);
    assert_eq!(
        nights[0].asleep_at, "2026-01-01T18:00:00Z",
        "23:30 IST, later than the 23:00 last event"
    );
    assert_eq!(nights[0].woke_at, "2026-01-02T00:30:00Z", "06:00 IST");
    assert_eq!(nights[0].seconds, 23_400);
}

#[test]
fn est_none_starts_at_nine_pm_local() {
    let events = vec![
        event(
            "last",
            "last",
            "2026-01-01T18:00:00+05:30",
            "2026-01-01T20:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
        event(
            "first",
            "first",
            "2026-01-02T09:00:00+05:30",
            "2026-01-02T10:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].source, SleepSource::EstNone);
    assert_eq!(nights[0].asleep_at, "2026-01-01T15:30:00Z", "21:00 IST");
    assert_eq!(nights[0].woke_at, "2026-01-01T22:00:00Z", "03:30 IST");
    assert_eq!(nights[0].seconds, 23_400);
}

#[test]
fn est_none_uses_nine_pm_when_the_previous_day_has_no_events() {
    let events = vec![event(
        "first",
        "first",
        "2026-01-02T09:00:00+05:30",
        "2026-01-02T10:00:00+05:30",
        "Asia/Kolkata",
        None,
    )];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].source, SleepSource::EstNone);
    assert_eq!(nights[0].asleep_at, "2026-01-01T15:30:00Z", "21:00 IST on D0");
}

#[test]
fn est_none_never_collapses_when_one_day_has_no_events() {
    // The legacy bug: `min(bed + dur, bed)` produced a zero-length night when
    // the other day had no events. With no first event on D1 the missing
    // anchor must fall back to the 6.5h default alone.
    let events = vec![marker("asleep", "Asleep", "2026-01-01T23:00:00+05:30", "2026-01-01T23:01:00+05:30")];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].source, SleepSource::EstNoWake);
    assert_eq!(nights[0].seconds, 23_400);
    assert_eq!(nights[0].woke_at, "2026-01-02T00:00:00Z", "05:30 IST");
}

#[test]
fn a_blank_past_stretch_does_not_invent_a_night() {
    // The backfill cursor walks every civil date from the first event to
    // today, so a long rebuild would otherwise store a fake 6.5h `est_none`
    // night on every blank day. `sleep_events.py` only inserted sleep between
    // days that had events. `NOW` is 2030, so the before-noon today-guard is
    // not what keeps these two 2020 dates empty.
    let dates = ["2020-06-01".to_string(), "2020-06-02".to_string()];
    let days = digest_days(&[], &dates, NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(days.len(), 2, "both requested dates are still emitted");
    for day in &days {
        assert_eq!(day.tracked_seconds, 0, "{}", day.local_date);
        assert_eq!(day.sleep_seconds, 0, "{}", day.local_date);
        assert!(day.nights.is_empty(), "{}", day.local_date);
    }
    assert_invariants(&days);
}

#[test]
fn a_post_midnight_asleep_credits_the_previous_night() {
    // 01:00 with no wake marker, first real event at 09:00 → D0's bedtime.
    let events = vec![
        marker("asleep", "Asleep", "2026-01-02T01:00:00+05:30", "2026-01-02T01:01:00+05:30"),
        event(
            "first",
            "first",
            "2026-01-02T09:00:00+05:30",
            "2026-01-02T10:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    let nights = all_nights(&days);
    assert_eq!(nights.len(), 1);
    assert_eq!(nights[0].source, SleepSource::EstNoWake);
    assert_eq!(nights[0].wake_date, "2026-01-02");
    assert_eq!(nights[0].asleep_at, "2026-01-01T19:30:00Z", "01:00 IST on the 2nd");
    assert_eq!(nights[0].woke_at, "2026-01-02T02:00:00Z", "07:30 IST");
    assert_eq!(nights[0].bed_min_past_noon, 780);
}

#[test]
fn a_collapsing_estimate_emits_nothing() {
    // A bedtime marker at 01:00 with D1's first real event at 00:00: the
    // estimate ends before it starts, so there is no night at all.
    let events = vec![
        event(
            "early",
            "early",
            "2026-01-02T00:00:00+05:30",
            "2026-01-02T00:30:00+05:30",
            "Asia/Kolkata",
            None,
        ),
        marker("asleep", "Asleep", "2026-01-02T01:00:00+05:30", "2026-01-02T01:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert!(all_nights(&days).is_empty(), "{:?}", all_nights(&days));
}

#[test]
fn a_paired_night_is_never_also_estimated() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:30:00+05:30", "2026-01-02T05:00:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T05:00:00+05:30", "2026-01-02T05:01:00+05:30"),
    ];
    // Both sides of the night are in the rebuild set.
    let dates = ["2026-01-01".to_string(), "2026-01-02".to_string()];
    let days = digest_days(
        &events,
        &dates,
        NOW,
        "Asia/Kolkata",
        None,
        None,
        DEFAULT_SLEEP_SECS,
    );
    let mornings = &find(&days, "2026-01-02").nights;
    assert_eq!(mornings.len(), 1, "one paired night on its wake date: {mornings:?}");
    assert_eq!(mornings[0].source, SleepSource::Paired);
    // The evening itself carries no paired night (only D1 = 2026-01-02 does).
    assert!(find(&days, "2026-01-01")
        .nights
        .iter()
        .all(|night| night.source != SleepSource::Paired));
}

#[test]
fn an_in_progress_morning_is_not_estimated_but_is_after_noon() {
    // 06:00 IST on 2026-01-02, no wake marker: the night may still be running.
    let now = utc("2026-01-02T00:30:00Z"); // 06:00 IST
    let events = vec![marker("asleep", "Asleep", "2026-01-01T23:00:00+05:30", "2026-01-01T23:01:00+05:30")];
    let days = digest_days(&events, &["2026-01-02".to_string()], now, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert!(
        all_nights(&days).is_empty(),
        "before noon on today: no estimate"
    );

    // 13:00 IST the same day: the night is over, estimate it.
    let now = utc("2026-01-02T07:30:00Z"); // 13:00 IST
    let days = digest_days(&events, &["2026-01-02".to_string()], now, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(all_nights(&days).len(), 1, "after noon: estimate");

    // A wake marker lifts the guard even in the morning.
    let now = utc("2026-01-02T00:30:00Z");
    let with_wake = vec![
        marker("asleep", "Asleep", "2026-01-01T23:00:00+05:30", "2026-01-01T23:01:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T06:00:00+05:30", "2026-01-02T06:01:00+05:30"),
    ];
    let days = digest_days(&with_wake, &["2026-01-02".to_string()], now, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(all_nights(&days).len(), 1);
}

#[test]
fn a_paired_night_is_emitted_even_on_this_morning() {
    let now = utc("2026-01-02T02:00:00Z"); // 07:30 IST
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:30:00+05:30", "2026-01-02T05:00:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T05:00:00+05:30", "2026-01-02T05:01:00+05:30"),
    ];
    let days = digest_days(&events, &["2026-01-02".to_string()], now, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(all_nights(&days).len(), 1);
}

#[test]
fn synthetic_sleep_uses_the_sleep_category_when_one_exists() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:30:00+05:30", "2026-01-02T05:00:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T05:00:00+05:30", "2026-01-02T05:01:00+05:30"),
    ];
    let days = digest_days(
        &events,
        &["2026-01-02".to_string()],
        NOW,
        "Asia/Kolkata",
        None,
        Some("cat-sleep"),
        DEFAULT_SLEEP_SECS,
    );
    let day = find(&days, "2026-01-02");
    assert_eq!(categories(day).get("cat-sleep"), Some(&9_000));
    assert_eq!(categories(day).get(SYNTHETIC_SLEEP_CATEGORY_ID), None);
}

// ──────────────────────────────────────────
// Dirty dates
// ──────────────────────────────────────────

#[test]
fn dirty_dates_cover_the_span_and_its_neighbours() {
    let dates = dirty_dates_for_span(
        "2026-01-01T10:00:00Z",
        "2026-01-01T11:00:00Z",
        "UTC",
        "",
    );
    assert_eq!(
        dates,
        vec![
            "2025-12-31".to_string(),
            "2026-01-01".to_string(),
            "2026-01-02".to_string()
        ]
    );
}

#[test]
fn dirty_dates_use_the_start_zone_and_the_end_zone_fallback() {
    // 22:00 IST on the 1st → 01:00 IST on the 2nd touches both dates.
    let dates = dirty_dates_for_span(
        "2026-01-01T22:00:00+05:30",
        "2026-01-02T01:00:00+05:30",
        "Asia/Kolkata",
        "",
    );
    assert_eq!(
        dates,
        vec![
            "2025-12-31".to_string(),
            "2026-01-01".to_string(),
            "2026-01-02".to_string(),
            "2026-01-03".to_string()
        ]
    );

    // Same instants, but dated in UTC: both stamps are on the 1st, so the
    // touched dates differ — the start zone decides.
    let utc = dirty_dates_for_span(
        "2026-01-01T22:00:00+05:30",
        "2026-01-02T01:00:00+05:30",
        "UTC",
        "",
    );
    assert_eq!(
        utc,
        vec![
            "2025-12-31".to_string(),
            "2026-01-01".to_string(),
            "2026-01-02".to_string()
        ]
    );

    // Empty start zone falls back to the end zone.
    let fallback = dirty_dates_for_span("2026-01-01T22:00:00+05:30", "2026-01-02T01:00:00+05:30", "", "Asia/Kolkata");
    assert_eq!(fallback, dates);
}

#[test]
fn dirty_dates_do_not_include_the_day_after_a_midnight_end() {
    // A four-day span: the half-open end at exactly midnight belongs to no one.
    let dates = dirty_dates_for_span(
        "2026-01-01T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "UTC",
        "",
    );
    assert_eq!(
        dates,
        vec![
            "2025-12-31".to_string(),
            "2026-01-01".to_string(),
            "2026-01-02".to_string(),
            "2026-01-03".to_string(),
            "2026-01-04".to_string(),
            "2026-01-05".to_string()
        ],
        "the 6th must not appear"
    );
}

#[test]
fn dirty_dates_reject_invalid_timestamps() {
    assert!(dirty_dates_for_span("nope", "2026-01-01T11:00:00Z", "UTC", "").is_empty());
    assert!(dirty_dates_for_span("2026-01-01T11:00:00Z", "", "UTC", "").is_empty());
}

#[test]
fn dirty_dates_union_old_and_new_spans() {
    let old = EventSpan {
        start_rfc3339: "2026-01-01T10:00:00Z",
        end_rfc3339: "2026-01-01T11:00:00Z",
        start_time_zone: "UTC",
        end_time_zone: "UTC",
    };
    let new = EventSpan {
        start_rfc3339: "2026-01-05T10:00:00Z",
        end_rfc3339: "2026-01-05T11:00:00Z",
        start_time_zone: "UTC",
        end_time_zone: "UTC",
    };
    assert_eq!(
        dirty_dates_for_spans(&[old, new]),
        vec![
            "2025-12-31".to_string(),
            "2026-01-01".to_string(),
            "2026-01-02".to_string(),
            "2026-01-04".to_string(),
            "2026-01-05".to_string(),
            "2026-01-06".to_string()
        ]
    );
}

#[test]
fn a_neighbor_wake_spills_its_evening_onto_the_rebuilt_day() {
    // No markers: the night is estimated from the days' real-event anchors.
    // The night that wakes on D + 1 starts at 21:00 on D, so its pre-midnight
    // half belongs to D — and estimation only walks the dates it is handed.
    let events = vec![
        ev("evening", "2026-01-02T19:00:00Z", "2026-01-02T20:00:00Z", Some("work")),
        ev("morning", "2026-01-03T09:00:00Z", "2026-01-03T10:00:00Z", Some("work")),
    ];

    // Asked for D alone, only the night that wakes on D exists (21:00 the
    // previous evening → 03:30, so 12_600s land on D after midnight).
    let alone = digest_days(&events, &["2026-01-02".to_string()], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(find(&alone, "2026-01-02").sleep_seconds, 12_600);

    // Asked for the neighbours too, the night that wakes on D + 1 spills its
    // 21:00–24:00 half (10_800s) onto D.
    let dates = [
        "2026-01-01".to_string(),
        "2026-01-02".to_string(),
        "2026-01-03".to_string(),
    ];
    let expanded = digest_days(&events, &dates, NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    let day = find(&expanded, "2026-01-02");
    assert_eq!(
        day.sleep_seconds,
        12_600 + 10_800,
        "the neighbour's evening half lands on the rebuilt day"
    );
    assert!(
        day.nights.iter().all(|night| night.wake_date == "2026-01-02"),
        "the spilled night still belongs to its wake date: {:?}",
        day.nights
    );
    assert_eq!(
        find(&expanded, "2026-01-03").nights.len(),
        1,
        "the night is owned by D + 1"
    );
    assert_invariants(&expanded);
}

// ──────────────────────────────────────────
// Facade shape
// ──────────────────────────────────────────

#[test]
fn an_empty_rebuild_still_produces_a_zero_day() {
    // Today before noon with no wake marker: the night may still be running,
    // so nothing is estimated and the rebuilt day is a clean zero.
    let now = utc("2026-01-02T06:00:00Z"); // 11:30 IST on 2026-01-02
    let days = digest_days(&[], &["2026-01-02".to_string()], now, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].tracked_seconds, 0);
    assert_eq!(days[0].sleep_seconds, 0);
    assert_eq!(days[0].time_zone, "Asia/Kolkata");
    assert!(days[0].category_seconds.is_empty());
    assert!(days[0].hour_seconds.is_empty());
    assert!(days[0].nights.is_empty());
    assert_eq!(days[0].computed_at, "2026-01-02T06:00:00Z");
    assert_eq!(ALGO_VERSION, 1);
}

#[test]
fn only_the_requested_dates_are_emitted() {
    let events = vec![
        ev("one", "2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z", Some("one")),
        ev("two", "2026-01-02T10:00:00Z", "2026-01-02T11:00:00Z", Some("two")),
    ];
    let days = digest_days(&events, &["2026-01-01".to_string()], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(days.len(), 1, "the neighbour must not be replaced");
    assert_eq!(days[0].local_date, "2026-01-01");

    let days = digest_days(&events, &[], NOW, "UTC", None, None, DEFAULT_SLEEP_SECS);
    assert_eq!(days.len(), 2, "no explicit dates ⇒ every date with content");
}

#[test]
fn nights_are_attached_to_their_wake_date() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:30:00+05:30", "2026-01-02T05:00:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T05:00:00+05:30", "2026-01-02T05:01:00+05:30"),
    ];
    // Rebuilding only the wake date moves that night and nothing else.
    let days = digest_days(
        &events,
        &["2026-01-02".to_string()],
        NOW,
        "Asia/Kolkata",
        None,
        None,
        DEFAULT_SLEEP_SECS,
    );
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].local_date, "2026-01-02");
    assert_eq!(days[0].nights.len(), 1);
    assert_eq!(days[0].nights[0].wake_date, "2026-01-02");
    assert_eq!(days[0].nights[0].source, SleepSource::Paired);
    assert_invariants(&days);
}

#[test]
fn the_pre_midnight_half_of_a_night_lands_on_the_previous_date() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:30:00+05:30", "2026-01-02T05:00:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T05:00:00+05:30", "2026-01-02T05:01:00+05:30"),
    ];
    let dates = ["2026-01-01".to_string(), "2026-01-02".to_string()];
    let days = digest_days(&events, &dates, NOW, "Asia/Kolkata", None, None, DEFAULT_SLEEP_SECS);
    // 22:30–24:00 IST shared between the Asleep event and the synthetic half
    // (2700s) — and nothing else: the 1st's only events are markers (excluded
    // from anchors and from the neither-marker estimate), and the 31st has no
    // events at all, so no `est_none` night is invented for it.
    assert_eq!(find(&days, "2026-01-01").sleep_seconds, 2_700);
    assert!(
        find(&days, "2026-01-01").nights.is_empty(),
        "a blank day must not own an estimated night"
    );
    // 00:00–05:00 IST: 5h synthetic, shared with the Asleep event's second half.
    assert_eq!(find(&days, "2026-01-02").sleep_seconds, 9_000);
    // The paired night itself is only ever attached to its wake date.
    assert_eq!(find(&days, "2026-01-02").nights.len(), 1);
    assert_eq!(find(&days, "2026-01-02").nights[0].source, SleepSource::Paired);
    assert_invariants(&days);
}

#[test]
fn a_wide_window_and_a_single_requested_date_still_finds_the_night() {
    let events = vec![
        marker("asleep", "Asleep", "2026-01-01T22:30:00+05:30", "2026-01-02T05:00:00+05:30"),
        marker("woke", "Woke Up", "2026-01-02T05:00:00+05:30", "2026-01-02T05:01:00+05:30"),
    ];
    let days = digest_days(
        &events,
        &["2026-01-02".to_string()],
        NOW,
        "Asia/Kolkata",
        None,
        None,
        DEFAULT_SLEEP_SECS,
    );
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].local_date, "2026-01-02");
    assert_eq!(days[0].nights.len(), 1, "the night is included for its wake_date");
}

// ──────────────────────────────────────────
// The estimation default is the user's own rule (F15)
// ──────────────────────────────────────────

#[test]
fn the_estimated_night_uses_the_callers_default_duration() {
    // The `est_none` fixture: last event 18:00–20:00 on D0, first 09:00 on D1.
    // The night starts at 21:00 local and is not clipped, so its length is
    // exactly the caller's default.
    let events = vec![
        event(
            "last",
            "last",
            "2026-01-01T18:00:00+05:30",
            "2026-01-01T20:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
        event(
            "first",
            "first",
            "2026-01-02T09:00:00+05:30",
            "2026-01-02T10:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
    ];
    let dates = ["2026-01-02".to_string()];

    let by_default = digest_days(&events, &dates, NOW, "Asia/Kolkata", None, None, 0);
    assert_eq!(
        all_nights(&by_default)[0].seconds,
        DEFAULT_SLEEP_SECS,
        "a non-positive default falls back to the constant"
    );

    let eight_hours = digest_days(&events, &dates, NOW, "Asia/Kolkata", None, None, 8 * 3600);
    let night = &all_nights(&eight_hours)[0];
    assert_eq!(night.seconds, 8 * 3600, "the rule's target wins, not 6.5h");
    assert_eq!(night.source, SleepSource::EstNone);
    assert_eq!(
        night.woke_at, "2026-01-01T23:30:00Z",
        "21:00 IST + 8h = 05:00 IST"
    );
    assert_invariants(&eight_hours);

    // `est_no_wake`: the bedtime is a marker, the wake is the default.
    let bed_only = vec![
        marker(
            "asleep",
            "Asleep",
            "2026-01-01T23:00:00+05:30",
            "2026-01-01T23:01:00+05:30",
        ),
        event(
            "first",
            "first",
            "2026-01-02T09:00:00+05:30",
            "2026-01-02T10:00:00+05:30",
            "Asia/Kolkata",
            None,
        ),
    ];
    let days = digest_days(&bed_only, &dates, NOW, "Asia/Kolkata", None, None, 8 * 3600);
    let night = &all_nights(&days)[0];
    assert_eq!(night.source, SleepSource::EstNoWake);
    assert_eq!(night.seconds, 8 * 3600, "23:00 + 8h = 07:00 IST");
}

#[test]
fn a_sleep_duration_revision_names_the_default_in_seconds() {
    assert_eq!(
        sleep_target_secs_from_params(r#"{"window_days":60,"min_nights":14,"target_hours":6.5}"#),
        Some(23_400)
    );
    assert_eq!(
        sleep_target_secs_from_params(r#"{"window_days":60,"min_nights":14,"target_hours":8}"#),
        Some(28_800)
    );
    // A fractional target rounds to the nearest second.
    assert_eq!(
        sleep_target_secs_from_params(r#"{"target_hours":7.75}"#),
        Some(27_900)
    );
    // Nothing usable: the caller keeps `DEFAULT_SLEEP_SECS`.
    assert_eq!(sleep_target_secs_from_params("not json"), None);
    assert_eq!(sleep_target_secs_from_params(r#"{"window_days":60}"#), None);
    assert_eq!(sleep_target_secs_from_params(r#"{"target_hours":0}"#), None);
    assert_eq!(
        sleep_target_secs_from_params(r#"{"target_hours":-3}"#),
        None
    );
}

// ──────────────────────────────────────────
// Statement limits and the fair work order (F14, F16)
// ──────────────────────────────────────────

#[test]
fn chunk_sizes_keep_a_bind_of_headroom() {
    // The arithmetic itself is a compile-time guard in `digest::repo`; these
    // pin the numbers the statements were built for.
    assert_eq!(DIGEST_MARK_DIRTY_CHUNK_SIZE, 33);
    assert_eq!(DIGEST_EVENT_SPANS_MAX_IDS, 99);
}

#[test]
fn the_work_order_is_oldest_dirty_first() {
    // The same user must not be picked every tick while others wait: the SQL
    // orders by the oldest dirty stamp, then by user id.
    assert!(
        super::repo::DIGEST_USERS_WITH_WORK_SQL.contains("ORDER BY"),
        "{}",
        super::repo::DIGEST_USERS_WITH_WORK_SQL
    );
    assert!(super::repo::DIGEST_USERS_WITH_WORK_SQL.contains("MIN(enqueued_at)"));
    assert!(super::repo::DIGEST_USERS_WITH_WORK_SQL.contains("user_id"));
}
