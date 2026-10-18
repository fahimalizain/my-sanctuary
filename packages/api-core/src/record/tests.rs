//! Pure assembler tests: no D1, no clock, no I/O.
//!
//! Each test pins one rule from the slice-5 brief — the root rollup, the gap
//! day, the share, the productive filter, the moving-average history gate, the
//! heatmap denominator, the paired-night median, and the unclassified bucket.

use std::collections::BTreeMap;

use crate::digest::shift_date;

use super::assemble::{assemble_record, RecordSegment};
use super::repo::{
    RecordCategoryRow, RecordCategorySecondsRow, RecordDaySecondsRow, RecordHourSecondsRow,
    RecordMetaRow, RecordNightRow, RecordTitleRow,
};
use super::{RankingRow, RecordData, RecordRange};

/// 2026-10-18T06:00:00Z — the UTC civil date is 2026-10-18, a Sunday.
const NOW_UNIX: i64 = 1_792_312_800;
const AS_OF: &str = "2026-10-18";
/// 273 days earlier (`273 % 7 == 0`, so also a Sunday): 2026-01-18T06:00:00Z.
const NOW_JANUARY: i64 = NOW_UNIX - 273 * 86_400;
const AS_OF_JANUARY: &str = "2026-01-18";

// ── fixtures ────────────────────────────────────────────────────────────────

fn cat(
    id: &str,
    parent: Option<&str>,
    productive: bool,
    title: &str,
    color: &str,
) -> RecordCategoryRow {
    RecordCategoryRow {
        id: id.to_string(),
        parent_id: parent.map(|parent| parent.to_string()),
        title: title.to_string(),
        color: color.to_string(),
        slug: id.to_string(),
        is_productive: productive,
    }
}

fn meta(date: &str, tracked: i64) -> RecordMetaRow {
    RecordMetaRow {
        local_date: date.to_string(),
        time_zone: "UTC".to_string(),
        tracked_seconds: tracked,
    }
}

fn secs(date: &str, category_id: &str, seconds: i64) -> RecordCategorySecondsRow {
    RecordCategorySecondsRow {
        local_date: date.to_string(),
        category_id: category_id.to_string(),
        seconds,
    }
}

fn hour(date: &str, hour: i64, category_id: &str, seconds: i64) -> RecordHourSecondsRow {
    RecordHourSecondsRow {
        local_date: date.to_string(),
        hour,
        category_id: category_id.to_string(),
        seconds,
    }
}

fn productive(date: &str, seconds: i64) -> RecordDaySecondsRow {
    RecordDaySecondsRow {
        local_date: date.to_string(),
        seconds,
    }
}

fn night(wake_date: &str, source: &str, seconds: i64) -> RecordNightRow {
    RecordNightRow {
        wake_date: wake_date.to_string(),
        source: source.to_string(),
        seconds,
    }
}

fn title(title: &str, count: i64) -> RecordTitleRow {
    RecordTitleRow {
        title: title.to_string(),
        count,
    }
}

fn segments_by_id(segments: &[RecordSegment]) -> BTreeMap<&str, &RecordSegment> {
    segments
        .iter()
        .map(|segment| (segment.category_id.as_str(), segment))
        .collect()
}

/// The work/family tree every rollup test shares.
fn work_tree() -> Vec<RecordCategoryRow> {
    vec![
        cat("work", None, true, "Work", "#204f0a"),
        cat("work-deep", Some("work"), false, "Deep work", "#123456"),
        cat("work-mail", Some("work"), false, "Email", "#654321"),
        cat("family", None, false, "Family", "#b02a2a"),
    ]
}

// ── 1. leaf rollup ──────────────────────────────────────────────────────────

#[test]
fn leaf_seconds_roll_up_to_the_parent_root() {
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-10-17", 10_800)],
        category_seconds: vec![
            secs("2026-10-17", "work-deep", 3_600),
            secs("2026-10-17", "work-mail", 7_200),
        ],
        productive_seconds: vec![productive("2026-10-17", 10_800)],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last30, NOW_UNIX);
    let day = report
        .days
        .iter()
        .find(|day| day.date == "2026-10-17")
        .expect("the day is in the range");

    assert_eq!(
        day.segments.len(),
        1,
        "the leaves are not segments of their own"
    );
    let segment = &day.segments[0];
    assert_eq!(segment.category_id, "work");
    assert_eq!(segment.title, "Work");
    assert_eq!(segment.color, "#204f0a");
    assert_eq!(segment.seconds, 10_800, "sibling leaves sum into the root");

    assert_eq!(report.ranking.len(), 1);
    assert_eq!(report.ranking[0].category_id, "work");
    assert_eq!(report.ranking[0].seconds, 10_800);
}

// ── 2. the gap day ──────────────────────────────────────────────────────────

#[test]
fn a_gap_day_with_no_aggregate_is_not_a_tracked_zero() {
    let data = RecordData {
        categories: work_tree(),
        // 2026-10-13 is deliberately missing: nothing has been computed for it.
        meta: vec![
            meta("2026-10-12", 3_600),
            meta("2026-10-14", 3_600),
            // Today, computed: no untracked figure because it is still running.
            meta(AS_OF, 0),
        ],
        category_seconds: vec![
            secs("2026-10-12", "work", 3_600),
            secs("2026-10-14", "work", 3_600),
        ],
        productive_seconds: vec![
            productive("2026-10-12", 3_600),
            productive("2026-10-14", 3_600),
        ],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);

    assert_eq!(report.from, "2026-10-12");
    assert_eq!(report.to, AS_OF);
    assert_eq!(report.days.len(), 7, "every civil date, zeros included");
    assert_eq!(report.days[0].date, "2026-10-12");
    assert_eq!(report.days[6].date, AS_OF);

    let gap = &report.days[1];
    assert_eq!(gap.date, "2026-10-13");
    assert!(gap.segments.is_empty());
    assert!(!gap.aggregated, "no daily_meta row: the date is unbuilt");
    assert_eq!(
        gap.untracked_seconds, None,
        "an unbuilt day is not a 24h untracked claim"
    );

    // A tracked past day is aggregated and subtracts its own seconds.
    assert!(report.days[0].aggregated);
    assert_eq!(report.days[0].untracked_seconds, Some(82_800));

    // Today is computed but still being written: no untracked figure at all.
    assert!(report.days[6].aggregated);
    assert_eq!(report.days[6].untracked_seconds, None);
}

#[test]
fn a_computed_zero_day_is_aggregated_and_fully_untracked() {
    // A `daily_meta` row with `tracked_seconds = 0` is a real answer — the day
    // was computed and nothing was tracked — so it gets the full fill, unlike a
    // date with no row at all.
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-10-13", 0)],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);
    let day = &report.days[1];
    assert_eq!(day.date, "2026-10-13");
    assert!(day.aggregated);
    assert_eq!(day.untracked_seconds, Some(86_400));
}

// ── 3. ranking share and the previous window ────────────────────────────────

#[test]
fn ranking_share_sums_to_one_and_reads_the_previous_window() {
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-10-10", 3_600)],
        category_seconds: vec![
            secs("2026-10-14", "work", 7_200),
            secs("2026-10-14", "family", 3_600),
            // Only in the previous window (2026-10-05 .. 2026-10-11).
            secs("2026-10-10", "work-deep", 3_600),
        ],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);
    assert_eq!(report.previous_from, "2026-10-05");
    assert_eq!(report.previous_to, "2026-10-11");

    let by_id: BTreeMap<&str, &RankingRow> = report
        .ranking
        .iter()
        .map(|row| (row.category_id.as_str(), row))
        .collect();
    assert_eq!(by_id["work"].seconds, 7_200);
    assert_eq!(
        by_id["work"].previous_seconds, 3_600,
        "rolled up, not per leaf"
    );
    assert_eq!(by_id["family"].seconds, 3_600);
    assert_eq!(by_id["family"].previous_seconds, 0);

    let share: f64 = report.ranking.iter().map(|row| row.share).sum();
    assert!((share - 1.0).abs() < 1e-9, "share sums to 1, got {share}");
    assert!((by_id["work"].share - 2.0 / 3.0).abs() < 1e-9);
    assert!((by_id["family"].share - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn a_root_that_only_appeared_before_is_kept_with_zero_seconds() {
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-10-10", 3_600)],
        category_seconds: vec![
            secs("2026-10-10", "family", 3_600),
            secs("2026-10-14", "work", 1_800),
        ],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);
    let by_id: BTreeMap<&str, &RankingRow> = report
        .ranking
        .iter()
        .map(|row| (row.category_id.as_str(), row))
        .collect();
    assert_eq!(by_id["family"].seconds, 0);
    assert_eq!(by_id["family"].previous_seconds, 3_600);
    assert_eq!(by_id["family"].share, 0.0);
    // Ranking order: this range's seconds first.
    assert_eq!(report.ranking[0].category_id, "work");
}

// ── 4. productivity ─────────────────────────────────────────────────────────

#[test]
fn a_non_productive_category_is_absent_from_the_trend_and_the_heatmap() {
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-10-17", 3_600)],
        category_seconds: vec![secs("2026-10-17", "family", 3_600)],
        hour_seconds: vec![hour("2026-10-17", 9, "family", 3_600)],
        // The repo's productive aggregate never includes `family`.
        productive_seconds: vec![],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);

    let day = report
        .days
        .iter()
        .find(|day| day.date == "2026-10-17")
        .expect("day");
    assert_eq!(day.segments.len(), 1, "the segment is still recorded");
    assert_eq!(day.segments[0].category_id, "family");

    let point = report
        .trend
        .iter()
        .find(|point| point.date == "2026-10-17")
        .expect("point");
    assert_eq!(
        point.hours, 0.0,
        "a non-productive category is not productive time"
    );

    // 2026-10-17 is a Saturday (`weekday == 5`).
    assert_eq!(report.heatmap[5].seconds[9], 0);
}

// ── 5. moving averages ──────────────────────────────────────────────────────

#[test]
fn ma30_is_null_until_thirty_days_of_history_exist() {
    let mut productive_seconds = Vec::new();
    let mut meta_rows = Vec::new();
    let mut category_seconds = Vec::new();
    for offset in 0..30 {
        let date = shift_date("2026-09-19", offset).expect("date");
        meta_rows.push(meta(&date, 3_600));
        category_seconds.push(secs(&date, "work", 3_600));
        productive_seconds.push(productive(&date, 3_600));
    }
    let data = RecordData {
        categories: work_tree(),
        meta: meta_rows,
        category_seconds,
        productive_seconds,
        earliest_date: Some("2026-09-19".to_string()),
        ..RecordData::default()
    };
    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last30, NOW_UNIX);
    assert_eq!(report.trend.len(), 30);
    assert_eq!(report.trend[0].date, "2026-09-19");

    // Day 29 of history is still one day short of a 30-day window.
    assert_eq!(report.trend[28].date, "2026-10-17");
    assert_eq!(report.trend[28].ma30, None);
    // Day 30 has the full window, and it is a real mean of ones.
    let ma30 = report.trend[29].ma30.expect("day 30 has a window");
    assert!((ma30 - 1.0).abs() < 1e-9, "30 days of 1h is 1h, got {ma30}");
    assert_eq!(
        report.trend[29].ma60, None,
        "60 days of history do not exist yet"
    );
}

#[test]
fn a_day_inside_a_short_view_still_has_ma30_when_history_reaches_back() {
    // 29 days before the 7-day view's first day: the view's first point is day
    // 30 of history, so its window is complete even though only 7 days are
    // visible.
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-10-12", 3_600)],
        category_seconds: vec![secs("2026-10-12", "work", 3_600)],
        productive_seconds: vec![productive("2026-10-12", 3_600)],
        earliest_date: Some("2026-09-13".to_string()),
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);
    assert_eq!(report.trend.len(), 7, "one point per visible day only");
    let first = &report.trend[0];
    assert_eq!(first.date, "2026-10-12");
    assert_eq!(first.hours, 1.0);
    // 1h inside a 30-day window, and the other 29 days count as 0.
    let ma30 = first.ma30.expect("29 earlier days are enough");
    assert!((ma30 - 1.0 / 30.0).abs() < 1e-9, "got {ma30}");

    // One day less of history and the window does not exist yet.
    let short = RecordData {
        earliest_date: Some("2026-09-14".to_string()),
        ..data.clone()
    };
    let report = assemble_record(&short, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);
    assert_eq!(report.trend[0].ma30, None);
}

// ── 6. heatmap ──────────────────────────────────────────────────────────────

#[test]
fn heatmap_means_over_every_occurrence_of_the_weekday() {
    // 2026-01-18 is a Sunday, so 2026-01-05 and 2026-01-12 are the range's only
    // two Mondays.
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-01-05", 3_600)],
        category_seconds: vec![secs("2026-01-05", "work", 3_600)],
        hour_seconds: vec![
            hour("2026-01-05", 9, "work", 3_600),
            // A Tuesday row must not leak into Monday's mean.
            hour("2026-01-06", 10, "work", 3_600),
        ],
        productive_seconds: vec![productive("2026-01-05", 3_600)],
        ..RecordData::default()
    };

    let report = assemble_record(
        &data,
        AS_OF_JANUARY,
        "UTC",
        RecordRange::YearToDate,
        NOW_JANUARY,
    );
    assert_eq!(report.as_of, AS_OF_JANUARY);
    assert_eq!(report.days.len(), 18, "January 1 through January 18");
    assert_eq!(report.days.last().expect("today").untracked_seconds, None);

    let monday = &report.heatmap[0];
    assert_eq!(monday.weekday, 0);
    assert_eq!(monday.seconds.len(), 24);
    // 3600 over two Mondays (the zero one still counts) = 1800.
    assert_eq!(monday.seconds[9], 1_800);
    // An hour with no row is zero, and another weekday's row never leaks in.
    assert_eq!(monday.seconds[8], 0);
    assert_eq!(monday.seconds[10], 0);
    // Tuesday has its own two occurrences, at hour 10.
    assert_eq!(report.heatmap[1].seconds[10], 1_800);
    assert_eq!(report.heatmap[1].seconds[9], 0);
}

// ── 7. years ────────────────────────────────────────────────────────────────

#[test]
fn years_median_reads_paired_nights_only() {
    let data = RecordData {
        categories: work_tree(),
        earliest_date: Some("2026-10-01".to_string()),
        nights: vec![
            night("2026-03-01", "paired", 21_600),
            night("2026-03-02", "paired", 25_200),
            // An estimated night is not a measured one.
            night("2026-03-03", "est_none", 86_400),
        ],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last30, NOW_UNIX);
    assert_eq!(report.years.len(), 1);
    let year = &report.years[0];
    assert_eq!(year.year, 2026);
    assert_eq!(year.days, 291, "January 1 through October 18");
    assert_eq!(year.paired_nights, 2);
    // Even count: the mean of the two middle values (6h and 7h).
    assert_eq!(year.sleep_median_hours, Some(6.5));
    assert_eq!(
        year.productive_hours_per_day, 0.0,
        "no productive seconds at all"
    );
    assert_eq!(year.weekday_hours, 0.0);
    assert_eq!(year.weekend_hours, 0.0);

    // Without the paired nights there is no median at all.
    let unmeasured = RecordData {
        nights: vec![night("2026-03-03", "est_none", 86_400)],
        ..data.clone()
    };
    let report = assemble_record(&unmeasured, AS_OF, "UTC", RecordRange::Last30, NOW_UNIX);
    assert_eq!(report.years[0].sleep_median_hours, None);
    assert_eq!(report.years[0].paired_nights, 0);
}

#[test]
fn years_split_weekday_and_weekend_means_and_cap_at_eight() {
    // 2026-10-12 is a Monday, 2026-10-17 a Saturday.
    let data = RecordData {
        categories: work_tree(),
        productive_seconds: vec![
            productive("2026-10-12", 3_600),
            productive("2026-10-17", 7_200),
        ],
        earliest_date: Some("2010-05-01".to_string()),
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last30, NOW_UNIX);
    assert_eq!(
        report.years.len(),
        8,
        "capped to the eight most recent years"
    );
    assert_eq!(report.years[0].year, 2019);
    assert_eq!(report.years[7].year, 2026);

    let year = &report.years[7];
    // 2026 runs Thursday Jan 1 → Sunday Oct 18: 207 Mon–Fri days and 84
    // weekend days, zero days included.
    assert_eq!(year.weekday_hours, 1.0 / 207.0);
    assert_eq!(year.weekend_hours, 2.0 / 84.0);
    assert_eq!(year.productive_hours_per_day, 3.0 / 291.0);
}

// ── 8. the unclassified bucket ──────────────────────────────────────────────

#[test]
fn unclassified_seconds_are_their_own_segment_and_are_not_productive() {
    let data = RecordData {
        categories: work_tree(),
        meta: vec![meta("2026-10-17", 1_800)],
        category_seconds: vec![secs("2026-10-17", "", 1_800)],
        hour_seconds: vec![hour("2026-10-17", 9, "", 1_800)],
        titles: vec![title("Bus to Kondotty", 3)],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);
    let day = report
        .days
        .iter()
        .find(|day| day.date == "2026-10-17")
        .expect("day");
    assert_eq!(day.segments.len(), 1);
    assert_eq!(day.segments[0].category_id, "");
    assert_eq!(day.segments[0].title, "Unclassified");
    assert_eq!(day.segments[0].color, "#c4c0b4");
    assert_eq!(day.segments[0].seconds, 1_800);
    // Tracked, not the untracked fill.
    assert_eq!(day.untracked_seconds, Some(86_400 - 1_800));

    assert_eq!(
        report
            .trend
            .iter()
            .find(|point| point.date == "2026-10-17")
            .expect("point")
            .hours,
        0.0
    );
    assert_eq!(report.heatmap[5].seconds[9], 0);

    assert_eq!(report.quality.unclassified_titles.len(), 1);
    assert_eq!(
        report.quality.unclassified_titles[0].title,
        "Bus to Kondotty"
    );
    assert_eq!(report.quality.unclassified_titles[0].count, 3);
}

// ── presentation and quality details ────────────────────────────────────────

#[test]
fn segments_render_unknown_ids_empty_colours_and_synthetic_sleep() {
    let data = RecordData {
        categories: vec![cat("work", None, true, "Work", "")],
        meta: vec![meta("2026-10-17", 5_400)],
        category_seconds: vec![
            secs("2026-10-17", "work", 1_800),
            secs("2026-10-17", "*sleep", 1_800),
            secs("2026-10-17", "deleted-cat", 1_800),
        ],
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last7, NOW_UNIX);
    let day = report
        .days
        .iter()
        .find(|day| day.date == "2026-10-17")
        .expect("day");
    let by_id = segments_by_id(&day.segments);
    assert_eq!(day.segments.len(), 3);

    assert_eq!(by_id["*sleep"].title, "Sleep");
    assert_eq!(by_id["*sleep"].color, "#6e7c8a");
    assert_eq!(by_id["deleted-cat"].title, "Unknown");
    assert_eq!(by_id["deleted-cat"].color, "#9a9a90");
    assert_eq!(by_id["work"].title, "Work");
    assert_eq!(by_id["work"].color, "#9a9a90", "an empty colour falls back");

    // Equal seconds sort by title: Sleep, Unknown, Work.
    let titles: Vec<&str> = day
        .segments
        .iter()
        .map(|segment| segment.title.as_str())
        .collect();
    assert_eq!(titles, vec!["Sleep", "Unknown", "Work"]);
}

#[test]
fn quality_lists_estimated_nights_oldest_first_and_caps_them() {
    // 70 estimated nights inside a 90-day view, plus one before it.
    let mut nights = vec![night("2026-07-01", "est_none", 23_400)];
    for offset in 0..70 {
        let date = shift_date("2026-08-01", offset).expect("date");
        nights.push(night(&date, "est_no_wake", 21_600));
    }
    let data = RecordData {
        categories: work_tree(),
        nights,
        ..RecordData::default()
    };

    let report = assemble_record(&data, AS_OF, "UTC", RecordRange::Last90, NOW_UNIX);
    assert_eq!(report.from, "2026-07-21");
    assert_eq!(report.quality.estimated_nights.len(), 60, "capped at 60");
    assert_eq!(report.quality.estimated_nights[0].wake_date, "2026-08-01");
    assert_eq!(report.quality.estimated_nights[0].source, "est_no_wake");
    assert_eq!(report.quality.estimated_nights[0].seconds, 21_600);
    assert_eq!(
        report
            .quality
            .estimated_nights
            .last()
            .expect("last")
            .wake_date,
        "2026-09-29"
    );
    // A night outside the selected range is not flagged here.
    assert!(!report
        .quality
        .estimated_nights
        .iter()
        .any(|night| night.wake_date == "2026-07-01"));
}

#[test]
fn the_previous_window_is_the_same_length_immediately_before() {
    // A year-to-date window in mid-January is short, so its previous window is
    // too (never a whole year).
    let window = RecordRange::YearToDate
        .window(AS_OF_JANUARY)
        .expect("window");
    assert_eq!(window.from, "2026-01-01");
    assert_eq!(window.to, AS_OF_JANUARY);
    assert_eq!(window.length(), 18);
    assert_eq!(window.previous_from, "2025-12-14");
    assert_eq!(window.previous_to, "2025-12-31");

    // The default range is 30 days.
    assert_eq!(RecordRange::default(), RecordRange::Last30);
    assert_eq!(RecordRange::parse("30d"), Some(RecordRange::Last30));
    assert_eq!(RecordRange::parse("7d"), Some(RecordRange::Last7));
    assert_eq!(RecordRange::parse("90d"), Some(RecordRange::Last90));
    assert_eq!(RecordRange::parse("ytd"), Some(RecordRange::YearToDate));
    assert_eq!(RecordRange::parse("all"), None);
    assert_eq!(RecordRange::parse("30D"), None, "exact match only");
}
