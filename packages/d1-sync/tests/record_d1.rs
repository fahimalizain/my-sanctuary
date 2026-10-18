//! The record report against the real Worker migrations (local SQLite = D1
//! schema).
//!
//! Applies `apps/worker/migrations/*.sql`, seeds a user, a category tree, a
//! few days of `daily_meta` / `daily_category_minutes` / `daily_hour_seconds`,
//! a night, and a handful of events, then drives the api-core record service
//! through [`SqliteRecordRepo`] and checks the report the Worker would return.
//!
//! What this test buys over the pure assembler tests: the productive join
//! (`c.is_productive = 1 OR p.is_productive = 1`) and the unclassified-title
//! filters run against the real schema and real affinities, and the bound
//! parameters of every record statement are exercised once.

use api_core::record::{record_report, RecordRange};
use d1_sync::{
    open_harness, seed_category_full, seed_category_seconds, seed_event, seed_hour_seconds,
    seed_meta, seed_night, seed_user_token_calendar, SeedOpts,
};

/// 2026-10-18T06:00:00Z — a UTC civil date of 2026-10-18.
const NOW_UNIX: i64 = 1_792_312_800;

#[test]
fn d1_record_round_trips_a_report_through_the_service() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
        seed_category_full(
            &conn, "u-1", "cat-work", None, "work", "Work", true, "#204f0a",
        )
        .expect("category");
        seed_category_full(
            &conn,
            "u-1",
            "cat-deep",
            Some("cat-work"),
            "deep",
            "Deep work",
            false,
            "#123456",
        )
        .expect("child category");
        seed_category_full(
            &conn,
            "u-1",
            "cat-family",
            None,
            "family",
            "Family",
            false,
            "#b02a2a",
        )
        .expect("category");

        // 2026-10-16 (Friday): a productive root stamped directly.
        // 2026-10-17 (Saturday): a productive child, a non-productive root,
        // and an unclassified hour.
        seed_category_seconds(&conn, "u-1", "2026-10-16", "cat-work", 3_600).expect("seconds");
        seed_category_seconds(&conn, "u-1", "2026-10-17", "cat-deep", 7_200).expect("seconds");
        seed_category_seconds(&conn, "u-1", "2026-10-17", "cat-family", 1_800).expect("seconds");
        seed_category_seconds(&conn, "u-1", "2026-10-18", "", 1_800).expect("seconds");
        seed_meta(&conn, "u-1", "2026-10-16", "UTC", 3_600, 0).expect("meta");
        seed_meta(&conn, "u-1", "2026-10-17", "UTC", 9_000, 0).expect("meta");
        seed_meta(&conn, "u-1", "2026-10-18", "UTC", 1_800, 0).expect("meta");
        seed_hour_seconds(&conn, "u-1", "2026-10-17", 9, "cat-deep", 7_200).expect("hour");
        seed_hour_seconds(&conn, "u-1", "2026-10-17", 9, "cat-family", 1_800).expect("hour");

        seed_night(
            &conn,
            "u-1",
            "2026-10-17",
            "2026-10-16T22:00:00Z",
            21_600,
            "paired",
        )
        .expect("night");
        seed_night(
            &conn,
            "u-1",
            "2026-10-18",
            "2026-10-17T23:00:00Z",
            23_400,
            "est_none",
        )
        .expect("night");

        // Two living unclassified events, plus one of every excluded shape.
        for (id, start, title) in [
            ("ev-1", "2026-10-17T08:00:00Z", "Bus to Kondotty"),
            ("ev-2", "2026-10-17T18:00:00Z", "Bus to Kondotty"),
        ] {
            seed_event(
                &conn, id, "cal-1", title, start, "", false, true, "", None, None,
            )
            .expect("event");
        }
        seed_event(
            &conn,
            "ev-cancelled",
            "cal-1",
            "Bus to Kondotty",
            "2026-10-17T08:00:00Z",
            "cancelled",
            false,
            true,
            "",
            None,
            None,
        )
        .expect("event");
        seed_event(
            &conn,
            "ev-allday",
            "cal-1",
            "Bus to Kondotty",
            "2026-10-17T08:00:00Z",
            "",
            true,
            true,
            "",
            None,
            None,
        )
        .expect("event");
        seed_event(
            &conn,
            "ev-shared",
            "cal-1",
            "Bus to Kondotty",
            "2026-10-17T08:00:00Z",
            "",
            false,
            false,
            "",
            None,
            None,
        )
        .expect("event");
        seed_event(
            &conn,
            "ev-master",
            "cal-1",
            "Bus to Kondotty",
            "2026-10-17T08:00:00Z",
            "",
            false,
            true,
            "",
            Some("RRULE:FREQ=WEEKLY"),
            None,
        )
        .expect("event");
        seed_event(
            &conn,
            "ev-ooo",
            "cal-1",
            "Bus to Kondotty",
            "2026-10-17T08:00:00Z",
            "",
            false,
            true,
            "outOfOffice",
            None,
            None,
        )
        .expect("event");
        seed_event(
            &conn,
            "ev-classified",
            "cal-1",
            "Bus to Kondotty",
            "2026-10-17T08:00:00Z",
            "",
            false,
            true,
            "",
            None,
            Some("cat-work"),
        )
        .expect("event");
        // Outside the selected range.
        seed_event(
            &conn,
            "ev-old",
            "cal-1",
            "Bus to Kondotty",
            "2026-09-01T08:00:00Z",
            "",
            false,
            true,
            "",
            None,
            None,
        )
        .expect("event");
    }

    let report = pollster::block_on(record_report(
        &h.record(),
        &h.digest(),
        "u-1",
        RecordRange::Last7,
        NOW_UNIX,
    ))
    .expect("report");

    assert_eq!(report.as_of, "2026-10-18");
    assert_eq!(report.time_zone, "UTC");
    assert_eq!(report.range, "7d");
    assert_eq!(report.from, "2026-10-12");
    assert_eq!(report.to, "2026-10-18");
    assert_eq!(report.previous_from, "2026-10-05");
    assert_eq!(report.previous_to, "2026-10-11");
    assert_eq!(report.days.len(), 7);

    // ── days ────────────────────────────────────────────────────────────────
    let saturday = report
        .days
        .iter()
        .find(|day| day.date == "2026-10-17")
        .expect("day");
    assert_eq!(saturday.segments.len(), 2, "Work (rolled up) and Family");
    assert_eq!(saturday.segments[0].category_id, "cat-work");
    assert_eq!(saturday.segments[0].title, "Work");
    assert_eq!(saturday.segments[0].color, "#204f0a");
    assert_eq!(saturday.segments[0].seconds, 7_200, "the child rolls up");
    assert_eq!(saturday.segments[1].category_id, "cat-family");
    assert_eq!(saturday.segments[1].seconds, 1_800);
    assert_eq!(saturday.untracked_seconds, Some(86_400 - 9_000));

    let gap = report
        .days
        .iter()
        .find(|day| day.date == "2026-10-13")
        .expect("day");
    assert!(gap.segments.is_empty());
    assert!(!gap.aggregated, "no daily_meta row was ever written");
    assert_eq!(
        gap.untracked_seconds, None,
        "an unbuilt day is not a 24h untracked claim"
    );
    assert!(
        saturday.aggregated,
        "the 17th has a daily_meta row of its own"
    );

    assert_eq!(
        report.days.last().expect("today").untracked_seconds,
        None,
        "today is still being written"
    );

    // ── ranking ─────────────────────────────────────────────────────────────
    let share: f64 = report.ranking.iter().map(|row| row.share).sum();
    assert!((share - 1.0).abs() < 1e-9, "share sums to 1, got {share}");
    let work = report
        .ranking
        .iter()
        .find(|row| row.category_id == "cat-work")
        .expect("work ranks");
    assert_eq!(work.seconds, 3_600 + 7_200);
    assert_eq!(work.previous_seconds, 0);

    // ── trend ───────────────────────────────────────────────────────────────
    let point = report
        .trend
        .iter()
        .find(|point| point.date == "2026-10-17")
        .expect("point");
    assert_eq!(point.hours, 2.0, "only the productive leaf counts");
    assert_eq!(point.ma30, None, "the history is only a few days old");
    assert_eq!(report.trend.len(), 7);

    // ── heatmap ─────────────────────────────────────────────────────────────
    // 2026-10-17 is a Saturday (`weekday == 5`), the range's only one.
    assert_eq!(
        report.heatmap[5].seconds[9], 7_200,
        "the non-productive hour is not counted"
    );
    assert_eq!(report.heatmap[5].seconds[10], 0);
    assert_eq!(report.heatmap[0].seconds[9], 0);

    // ── quality ─────────────────────────────────────────────────────────────
    assert_eq!(report.quality.estimated_nights.len(), 1);
    assert_eq!(report.quality.estimated_nights[0].wake_date, "2026-10-18");
    assert_eq!(report.quality.estimated_nights[0].source, "est_none");
    assert_eq!(report.quality.unclassified_titles.len(), 1);
    assert_eq!(
        report.quality.unclassified_titles[0].title,
        "Bus to Kondotty"
    );
    assert_eq!(report.quality.unclassified_titles[0].count, 2);

    // ── years ───────────────────────────────────────────────────────────────
    assert_eq!(report.years.len(), 1);
    let year = &report.years[0];
    assert_eq!(year.year, 2026);
    assert_eq!(year.days, 291, "January 1 through October 18");
    assert_eq!(year.paired_nights, 1, "the estimated night is not measured");
    assert_eq!(year.sleep_median_hours, Some(6.0));
    // 3h productive (1h + 2h) over 291 days.
    assert!((year.productive_hours_per_day - 3.0 / 291.0).abs() < 1e-12);
}

#[test]
fn d1_record_is_all_zeros_before_the_first_rebuild() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
    }

    let report = pollster::block_on(record_report(
        &h.record(),
        &h.digest(),
        "u-1",
        RecordRange::YearToDate,
        NOW_UNIX,
    ))
    .expect("report");

    assert_eq!(report.range, "ytd");
    assert_eq!(report.from, "2026-01-01");
    assert!(report.ranking.is_empty());
    assert!(report.quality.estimated_nights.is_empty());
    assert!(report.quality.unclassified_titles.is_empty());
    // The year table still renders the running year, all zeros.
    assert_eq!(report.years.len(), 1);
    assert_eq!(report.years[0].year, 2026);
    assert_eq!(report.years[0].paired_nights, 0);
    assert_eq!(report.years[0].sleep_median_hours, None);
    assert_eq!(report.years[0].productive_hours_per_day, 0.0);
    assert_eq!(report.heatmap.len(), 7);
    assert!(report
        .heatmap
        .iter()
        .all(|row| row.seconds.iter().all(|s| *s == 0)));
}
