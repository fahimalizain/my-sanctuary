//! Pure 14-day consumption tests: no D1, no clock, no I/O.
//!
//! Each test pins one rule from the slice-1 brief: the fixed slug grammar and
//! its platform stems, the named-video collapse, the WhatsApp bucket, the
//! digest filter as reached through the assembler, the event's own zone for
//! dating, the full-duration credit across midnight, and the deterministic
//! ordering.

use crate::digest::DigestEvent;
use crate::record::{RecordCategoryRow, RecordError};
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};

use super::assemble::{assemble_consumption, ConsumptionReport};
use super::service::consumption_window;

const FROM: &str = "2026-11-05";
const TO: &str = "2026-11-18";
const AS_OF: &str = "2026-11-18";
const ZONE: &str = "Asia/Kolkata";

const CAT_MOVIES: &str = "cat-movies";
const CAT_GAMING: &str = "cat-gaming";
const CAT_YOUTUBE: &str = "cat-youtube";
const CAT_SOCIAL: &str = "cat-social";
const CAT_APPLIX: &str = "cat-applix";
const CAT_LEARNING: &str = "cat-learning";

// ── fixtures ────────────────────────────────────────────────────────────────

fn category(id: &str, slug: &str) -> RecordCategoryRow {
    RecordCategoryRow {
        id: id.to_string(),
        parent_id: None,
        title: slug.to_string(),
        color: String::new(),
        slug: slug.to_string(),
        is_productive: false,
    }
}

fn categories() -> Vec<RecordCategoryRow> {
    vec![
        category(CAT_MOVIES, "movies"),
        category(CAT_GAMING, "gaming"),
        category(CAT_YOUTUBE, "social-media-youtube"),
        category(CAT_SOCIAL, "social"),
        category(CAT_APPLIX, "applix"),
        category(CAT_LEARNING, "learning"),
    ]
}

/// A fully countable event in [`ZONE`] — every filter passes unless a test
/// breaks one field on purpose.
fn event(
    title: &str,
    start_rfc3339: &str,
    end_rfc3339: &str,
    category_id: Option<&str>,
) -> DigestEvent {
    DigestEvent {
        id: format!("{title}@{start_rfc3339}"),
        title: title.to_string(),
        start_rfc3339: start_rfc3339.to_string(),
        end_rfc3339: end_rfc3339.to_string(),
        start_time_zone: ZONE.to_string(),
        end_time_zone: ZONE.to_string(),
        category_id: category_id.map(str::to_string),
        is_all_day: false,
        is_self_organized: true,
        event_type: "default".to_string(),
        recurrence: String::new(),
        status: "confirmed".to_string(),
        deleted: false,
    }
}

/// Convenience: an event of `seconds` starting at `start` (the end is stamped
/// in UTC, which is the same instant and a valid RFC 3339 row).
fn span(title: &str, start: &str, seconds: i64, category_id: Option<&str>) -> DigestEvent {
    let start_unix = rfc3339_to_unix_secs(start).expect("fixture start is RFC 3339");
    event(
        title,
        start,
        &unix_secs_to_rfc3339(start_unix + seconds),
        category_id,
    )
}

fn report(events: &[DigestEvent]) -> ConsumptionReport {
    assemble_consumption(events, &categories(), FROM, TO, AS_OF, ZONE)
}

// ── the slug grammar ────────────────────────────────────────────────────────

#[test]
fn series_platform_is_the_stem_before_the_last_pipe() {
    let report = report(&[span(
        "Dar-e-Nijaat | TVSeries",
        "2026-11-10T09:00:00+05:30",
        3600,
        Some(CAT_MOVIES),
    )]);

    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.platform, "Dar-e-Nijaat");
    assert_eq!(row.group, "series");
    assert_eq!(row.seconds, 3600);
    assert_eq!(row.sessions, 1);
    assert_eq!(row.longest_seconds, 3600);
    assert_eq!(report.total_seconds, 3600);
}

#[test]
fn gaming_platform_is_the_stem_before_the_first_slash() {
    let report = report(&[
        span(
            "FC26 / PS5 | Game",
            "2026-11-10T09:00:00+05:30",
            3600,
            Some(CAT_GAMING),
        ),
        span(
            "Mob Control / iOS | Game",
            "2026-11-10T12:00:00+05:30",
            1800,
            Some(CAT_GAMING),
        ),
        span(
            "GTA6 Start Downloading / PS5 | Game",
            "2026-11-10T14:00:00+05:30",
            7200,
            Some(CAT_GAMING),
        ),
    ]);

    let platforms: Vec<&str> = report
        .rows
        .iter()
        .map(|row| row.platform.as_str())
        .collect();
    // Seconds descending: GTA6 2h, FC26 1h, Mob Control 30m. The game keeps its
    // logged name — no renaming to "EA FC 26" or "GTA6 setup".
    assert_eq!(platforms, ["GTA6 Start Downloading", "FC26", "Mob Control"]);
    assert!(report.rows.iter().all(|row| row.group == "gaming"));
}

#[test]
fn bare_twitter_and_x_com_are_one_platform() {
    let report = report(&[
        span(
            "Twitter",
            "2026-11-10T09:00:00+05:30",
            3600,
            Some(CAT_YOUTUBE),
        ),
        span(
            "x.com",
            "2026-11-10T11:00:00+05:30",
            1800,
            Some(CAT_YOUTUBE),
        ),
    ]);

    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.platform, "X / Twitter");
    assert_eq!(row.seconds, 5400);
    assert_eq!(row.sessions, 2);
    assert_eq!(row.longest_seconds, 3600);
}

#[test]
fn named_video_titles_collapse_into_one_bucket() {
    let report = report(&[
        span(
            "Gardener vs Carpenter Mindset | YouTube",
            "2026-11-10T09:00:00+05:30",
            3600,
            Some(CAT_YOUTUBE),
        ),
        span(
            "Kidneys | Youtube",
            "2026-11-10T11:00:00+05:30",
            1800,
            Some(CAT_YOUTUBE),
        ),
    ]);

    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.platform, "YouTube (named videos)");
    assert_eq!(row.seconds, 5400);
    assert_eq!(row.sessions, 2);
}

#[test]
fn a_platform_named_as_its_own_stem_stays_canonical() {
    let report = report(&[span(
        "YouTube | YouTube",
        "2026-11-10T09:00:00+05:30",
        3600,
        Some(CAT_YOUTUBE),
    )]);

    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].platform, "YouTube");
}

#[test]
fn a_platform_suffix_is_canonical() {
    let report = report(&[span(
        "foo | Twitter",
        "2026-11-10T09:00:00+05:30",
        3600,
        Some(CAT_YOUTUBE),
    )]);

    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].platform, "X / Twitter");
}

#[test]
fn whatsapp_counts_only_on_the_social_slug() {
    let report = report(&[
        span(
            "Whatsapp | Social",
            "2026-11-10T09:00:00+05:30",
            1800,
            Some(CAT_SOCIAL),
        ),
        span(
            "Akhil / Whatsapp Call | Social",
            "2026-11-10T10:00:00+05:30",
            3600,
            Some(CAT_SOCIAL),
        ),
        // Same WhatsApp title grammar, but the slug is not `social`: excluded.
        span(
            "Srini / Whatsapp Call | Applix",
            "2026-11-10T12:00:00+05:30",
            1800,
            Some(CAT_APPLIX),
        ),
    ]);

    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.platform, "WhatsApp");
    assert_eq!(row.group, "messaging");
    assert_eq!(row.seconds, 5400);
    assert_eq!(row.sessions, 2);
}

// ── the digest filter, through the assembler ────────────────────────────────

#[test]
fn a_focus_time_row_never_counts() {
    let mut focus = span(
        "Focus | TVSeries",
        "2026-11-10T09:00:00+05:30",
        3600,
        Some(CAT_MOVIES),
    );
    focus.event_type = "focusTime".to_string();

    let report = report(&[focus]);

    assert!(report.rows.is_empty());
    assert!(report.groups.is_empty());
    assert_eq!(report.total_seconds, 0);
}

#[test]
fn the_digest_filters_apply_through_the_assembler() {
    let mut all_day = span(
        "All Day | TVSeries",
        "2026-11-10T09:00:00+05:30",
        3600,
        Some(CAT_MOVIES),
    );
    all_day.is_all_day = true;

    let mut cancelled = span(
        "Cancelled | TVSeries",
        "2026-11-10T10:00:00+05:30",
        3600,
        Some(CAT_MOVIES),
    );
    cancelled.status = "cancelled".to_string();

    let mut recurring = span(
        "Recurring | TVSeries",
        "2026-11-10T11:00:00+05:30",
        3600,
        Some(CAT_MOVIES),
    );
    recurring.recurrence = "RRULE:FREQ=WEEKLY".to_string();

    let mut borrowed = span(
        "Borrowed | TVSeries",
        "2026-11-10T12:00:00+05:30",
        3600,
        Some(CAT_MOVIES),
    );
    borrowed.is_self_organized = false;

    let kept = span(
        "Kept | TVSeries",
        "2026-11-10T13:00:00+05:30",
        3600,
        Some(CAT_MOVIES),
    );

    let report = report(&[all_day, cancelled, recurring, borrowed, kept]);

    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].seconds, 3600);
    assert_eq!(report.rows[0].sessions, 1);
}

#[test]
fn unknown_slugs_and_unclassified_rows_do_not_count() {
    let report = report(&[
        // A normal event, but the slug is outside the fixed grammar.
        span(
            "Advanced Rust | Learning",
            "2026-11-10T09:00:00+05:30",
            3600,
            Some(CAT_LEARNING),
        ),
        // Unclassified: no category to map to a slug at all.
        span(
            "Dar-e-Nijaat | TVSeries",
            "2026-11-10T10:00:00+05:30",
            3600,
            None,
        ),
    ]);

    assert!(report.rows.is_empty());
    assert_eq!(report.total_seconds, 0);
}

// ── dating and duration ──────────────────────────────────────────────────────

#[test]
fn a_midnight_crossing_block_is_credited_once_in_full() {
    let report = report(&[event(
        "Late Movie | TVSeries",
        "2026-11-11T23:30:00+05:30",
        "2026-11-12T02:00:00+05:30",
        Some(CAT_MOVIES),
    )]);

    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].seconds, 9000);
    assert_eq!(report.rows[0].sessions, 1);
    assert_eq!(report.rows[0].longest_seconds, 9000);
}

#[test]
fn an_event_that_starts_before_the_window_is_excluded() {
    let report = report(&[event(
        "Early | TVSeries",
        "2026-11-04T22:00:00+05:30",
        "2026-11-05T02:00:00+05:30",
        Some(CAT_MOVIES),
    )]);

    assert!(report.rows.is_empty());
    assert_eq!(report.total_seconds, 0);
}

#[test]
fn dating_uses_the_events_own_zone_not_the_report_zone() {
    // The same instant twice: 2026-11-05T00:30:00+05:30 == 2026-11-04T19:00Z.
    let ist = event(
        "One | TVSeries",
        "2026-11-05T00:30:00+05:30",
        "2026-11-05T01:30:00+05:30",
        Some(CAT_MOVIES),
    );
    let mut utc = event(
        "Two | TVSeries",
        "2026-11-04T19:00:00Z",
        "2026-11-04T20:00:00Z",
        Some(CAT_MOVIES),
    );
    utc.start_time_zone = "UTC".to_string();
    utc.end_time_zone = "UTC".to_string();

    let report = report(&[ist, utc]);

    // The IST row is dated 2026-11-05 (in the window); the UTC row is dated
    // 2026-11-04 (before `from`) and drops even though it ends inside it.
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].platform, "One");
    assert_eq!(report.rows[0].seconds, 3600);
}

#[test]
fn zero_and_negative_durations_are_skipped() {
    let report = report(&[
        event(
            "Empty | TVSeries",
            "2026-11-10T09:00:00+05:30",
            "2026-11-10T09:00:00+05:30",
            Some(CAT_MOVIES),
        ),
        event(
            "Backwards | TVSeries",
            "2026-11-10T10:00:00+05:30",
            "2026-11-10T09:00:00+05:30",
            Some(CAT_MOVIES),
        ),
    ]);

    assert!(report.rows.is_empty());
    assert_eq!(report.total_seconds, 0);
}

// ── ordering ────────────────────────────────────────────────────────────────

#[test]
fn rows_and_groups_sort_by_seconds_then_name() {
    let report = report(&[
        span(
            "FC26 / PS5 | Game",
            "2026-11-10T09:00:00+05:30",
            7200,
            Some(CAT_GAMING),
        ),
        span(
            "YouTube",
            "2026-11-10T12:00:00+05:30",
            7200,
            Some(CAT_YOUTUBE),
        ),
        span(
            "Reddit",
            "2026-11-10T14:00:00+05:30",
            1800,
            Some(CAT_YOUTUBE),
        ),
        span(
            "Dar-e-Nijaat | TVSeries",
            "2026-11-10T16:00:00+05:30",
            3600,
            Some(CAT_MOVIES),
        ),
    ]);

    let rows: Vec<(&str, i64)> = report
        .rows
        .iter()
        .map(|row| (row.platform.as_str(), row.seconds))
        .collect();
    // 7200 tie broken by platform ascending ("FC26" < "YouTube").
    assert_eq!(
        rows,
        [
            ("FC26", 7200),
            ("YouTube", 7200),
            ("Dar-e-Nijaat", 3600),
            ("Reddit", 1800),
        ]
    );

    let groups: Vec<(&str, i64)> = report
        .groups
        .iter()
        .map(|group| (group.group.as_str(), group.seconds))
        .collect();
    // `messaging` never appears: an empty group is omitted, not a zero row.
    assert_eq!(
        groups,
        [("social", 9000), ("gaming", 7200), ("series", 3600)]
    );
    assert_eq!(report.total_seconds, 19800);
}

// ── the window ──────────────────────────────────────────────────────────────

#[test]
fn the_window_is_fourteen_days_with_two_days_of_load_slack() {
    let window = consumption_window("2026-11-18").expect("a valid civil date");

    assert_eq!(window.from, "2026-11-05");
    assert_eq!(window.to, "2026-11-18");
    assert_eq!(window.load_from, "2026-11-03");
    assert_eq!(window.load_to, "2026-11-20");
}

#[test]
fn a_malformed_as_of_is_invalid() {
    assert!(matches!(
        consumption_window("not-a-date"),
        Err(RecordError::Invalid(_))
    ));
}
