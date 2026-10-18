//! The pure record assembler: loaded aggregate rows in, [`RecordReport`] out.
//!
//! No I/O, no clock, no D1 — the service loads the rows and this module folds
//! them. Every ordering that reaches the client is deterministic, so the same
//! rows always render the same report.
//!
//! # Rollup
//!
//! `daily_category_minutes` is keyed by the **leaf** an event was stamped with
//! (`''` = unclassified real time, `*sleep` = synthetic sleep). A segment, a
//! ranking row, and a root's identity are all the leaf's **parent** when it has
//! one, else the leaf itself — one level, exactly like
//! [`CategoryCatalog::productive`](crate::rules::CategoryCatalog::productive).
//! Sibling leaves therefore sum into one root row, and the leaf never shows up
//! beside it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use chrono::Datelike;
use serde::Serialize;

use crate::digest::{shift_date, untracked_seconds, SYNTHETIC_SLEEP_CATEGORY_ID};
use crate::rules::{Cat, CategoryCatalog};

use super::repo::{
    RecordCategoryRow, RecordCategorySecondsRow, RecordDaySecondsRow, RecordHourSecondsRow,
    RecordMetaRow, RecordNightRow, RecordTitleRow,
};
use super::{days_between, first_year, parse_date, RecordRange, RecordWindow};

/// Estimated nights `quality` lists at most.
pub const ESTIMATED_NIGHTS_CAP: usize = 60;
/// Unclassified titles `quality` lists at most.
pub const UNCLASSIFIED_TITLES_CAP: usize = 8;
/// Calendar years the `years` table shows at most.
pub const YEARS_CAP: i32 = 8;

/// Title and colour of the unclassified bucket (`''`).
const UNCLASSIFIED_TITLE: &str = "Unclassified";
const UNCLASSIFIED_COLOR: &str = "#c4c0b4";
/// Title and colour of a deleted/unknown category id.
const UNKNOWN_TITLE: &str = "Unknown";
const UNKNOWN_COLOR: &str = "#9a9a90";
/// Title and colour of synthetic sleep (`*sleep`).
const SLEEP_TITLE: &str = "Sleep";
const SLEEP_COLOR: &str = "#6e7c8a";
/// Seconds in an hour, for the derived means.
const HOUR_SECONDS: f64 = 3600.0;

// ──────────────────────────────────────────
// Outputs
// ──────────────────────────────────────────

/// The `GET /api/reports/record` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordReport {
    pub as_of: String,
    pub time_zone: String,
    pub range: String,
    pub from: String,
    pub to: String,
    pub previous_from: String,
    pub previous_to: String,
    /// Every civil date in `[from, to]`, oldest first — zeros included.
    pub days: Vec<RecordDay>,
    /// The range's root totals, with the previous window beside them.
    pub ranking: Vec<RankingRow>,
    /// One point per day in the selected range (`ma30` / `ma60` look further
    /// back; `null` until the user's history reaches that far).
    pub trend: Vec<TrendPoint>,
    /// Seven rows, `weekday` `0 = Monday … 6 = Sunday`, 24 hours each.
    pub heatmap: Vec<HeatmapRow>,
    pub quality: RecordQuality,
    /// One row per calendar year, oldest first, capped at [`YEARS_CAP`].
    pub years: Vec<YearRow>,
}

/// One root category's seconds on one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordSegment {
    pub category_id: String,
    pub title: String,
    pub color: String,
    pub seconds: i64,
}

/// One day of the record. `untracked_seconds` is `null` while the day is still
/// being written (it is today in its own zone) and `null` for a day with no
/// aggregate at all (`aggregated == false`): a day nobody computed is not a
/// 24-hour untracked claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordDay {
    pub date: String,
    /// Whether a `daily_meta` row exists for the date. `false` = the date was
    /// zero-filled because nothing has been computed yet — a missing
    /// aggregate, not a computed zero.
    pub aggregated: bool,
    pub segments: Vec<RecordSegment>,
    pub untracked_seconds: Option<i64>,
}

/// One root category over the selected range, next to the previous window.
/// There is deliberately no verdict field: the delta is the reader's.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RankingRow {
    pub category_id: String,
    pub title: String,
    pub color: String,
    pub seconds: i64,
    pub previous_seconds: i64,
    /// `seconds` over the sum of this range's ranking rows (`0.0` when that
    /// sum is 0).
    pub share: f64,
}

/// One day of the productive trend.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrendPoint {
    pub date: String,
    pub hours: f64,
    pub ma30: Option<f64>,
    pub ma60: Option<f64>,
}

/// One weekday's mean productive seconds per clock hour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeatmapRow {
    pub weekday: u32,
    /// 24 means, `0` for an hour with no row.
    pub seconds: Vec<i64>,
}

/// One night whose length was estimated rather than paired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EstimatedNight {
    pub wake_date: String,
    pub source: String,
    pub seconds: i64,
}

/// One unclassified title and how often it occurred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnclassifiedTitle {
    pub title: String,
    pub count: i64,
}

/// What the aggregates could not classify. Deliberately not a judgement —
/// estimated nights and unfiled titles are both "flag it or don't".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordQuality {
    pub estimated_nights: Vec<EstimatedNight>,
    pub unclassified_titles: Vec<UnclassifiedTitle>,
}

/// One calendar year's descriptive means. `sleep_median_hours` is `null` until
/// the year has a paired night.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct YearRow {
    pub year: i32,
    /// Total productive hours over `days` (zero days count).
    pub productive_hours_per_day: f64,
    /// Mean productive hours on Mon–Fri (`0.0` when the year has none).
    pub weekday_hours: f64,
    /// Mean productive hours on Sat–Sun (`0.0` when the year has none).
    pub weekend_hours: f64,
    /// Median of the **paired** nights' lengths (an even count averages the
    /// two middle values, like the rules scorer).
    pub sleep_median_hours: Option<f64>,
    pub paired_nights: i64,
    /// January 1 through December 31, clipped to `as_of`.
    pub days: i64,
}

// ──────────────────────────────────────────
// Inputs
// ──────────────────────────────────────────

/// Everything [`assemble_record`] reads, already loaded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordData {
    /// The user's living categories.
    pub categories: Vec<RecordCategoryRow>,
    /// `daily_meta` over the selected range.
    pub meta: Vec<RecordMetaRow>,
    /// `daily_category_minutes` over the selected range **and** the previous
    /// window.
    pub category_seconds: Vec<RecordCategorySecondsRow>,
    /// `daily_hour_seconds` over the selected range.
    pub hour_seconds: Vec<RecordHourSecondsRow>,
    /// Per-day productive seconds: the trend windows and every year row.
    pub productive_seconds: Vec<RecordDaySecondsRow>,
    /// `sleep_nights` over the years window (every source).
    pub nights: Vec<RecordNightRow>,
    /// The earliest aggregate day the user has (`daily_meta` or a night), or
    /// `None` before the first rebuild.
    pub earliest_date: Option<String>,
    /// The unclassified titles the repo already grouped and ranked.
    pub titles: Vec<RecordTitleRow>,
}

// ──────────────────────────────────────────
// The assembler
// ──────────────────────────────────────────

/// Folds the loaded rows into the report for `range` ending on `as_of`.
///
/// `time_zone` is the user's primary calendar zone (already normalized, else
/// `UTC`); it is the day zone a date with no `daily_meta` row falls back to.
/// `now_unix` only decides whether the last day's untracked fill is `null`.
pub fn assemble_record(
    data: &RecordData,
    as_of: &str,
    time_zone: &str,
    range: RecordRange,
    now_unix: i64,
) -> RecordReport {
    let catalog = RecordCatalog::new(&data.categories);
    let window = range.window(as_of).unwrap_or_else(|| RecordWindow {
        from: as_of.to_string(),
        to: as_of.to_string(),
        previous_from: as_of.to_string(),
        previous_to: as_of.to_string(),
    });

    // ── Root rollup ─────────────────────────────────────────────────────────
    //
    // One pass over the category rows fills the per-day segments, the range
    // totals, and the previous-window totals at once.
    let mut per_date: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut range_totals: BTreeMap<String, i64> = BTreeMap::new();
    let mut previous_totals: BTreeMap<String, i64> = BTreeMap::new();
    for row in &data.category_seconds {
        let date = row.local_date.as_str();
        if date >= window.from.as_str() && date <= window.to.as_str() {
            let root = catalog.root(&row.category_id);
            *per_date
                .entry(row.local_date.clone())
                .or_default()
                .entry(root.clone())
                .or_insert(0) += row.seconds;
            *range_totals.entry(root).or_insert(0) += row.seconds;
        } else if date >= window.previous_from.as_str() && date <= window.previous_to.as_str() {
            *previous_totals
                .entry(catalog.root(&row.category_id))
                .or_insert(0) += row.seconds;
        }
    }

    let meta: BTreeMap<&str, &RecordMetaRow> = data
        .meta
        .iter()
        .map(|row| (row.local_date.as_str(), row))
        .collect();

    // ── days ────────────────────────────────────────────────────────────────
    let mut days: Vec<RecordDay> = Vec::new();
    for date in each_date(&window.from, &window.to) {
        let day_meta = meta.get(date.as_str());
        // A missing row is **not** a tracked zero: nothing has been computed
        // for the date yet (`aggregated: false`), so there is no aggregate to
        // subtract from and no honest untracked figure either.
        let tracked = day_meta.map_or(0, |row| row.tracked_seconds);
        let day_zone = day_meta
            .map(|row| row.time_zone.trim())
            .filter(|zone| !zone.is_empty())
            .unwrap_or(time_zone);
        let untracked = match day_meta {
            None => None,
            Some(_) => untracked_seconds(tracked, &date, day_zone, now_unix),
        };
        days.push(RecordDay {
            aggregated: day_meta.is_some(),
            segments: per_date
                .get(&date)
                .map(|totals| segments_for(&catalog, totals))
                .unwrap_or_default(),
            untracked_seconds: untracked,
            date,
        });
    }

    // ── ranking ─────────────────────────────────────────────────────────────
    let mut ids: BTreeSet<String> = range_totals.keys().cloned().collect();
    ids.extend(previous_totals.keys().cloned());
    let mut ranking: Vec<RankingRow> = ids
        .into_iter()
        .filter_map(|category_id| {
            let seconds = range_totals.get(&category_id).copied().unwrap_or(0);
            let previous_seconds = previous_totals.get(&category_id).copied().unwrap_or(0);
            // Absent from both windows is not a row at all.
            if seconds == 0 && previous_seconds == 0 {
                return None;
            }
            let (title, color) = catalog.display(&category_id);
            Some(RankingRow {
                category_id,
                title,
                color,
                seconds,
                previous_seconds,
                share: 0.0,
            })
        })
        .collect();
    ranking.sort_by(|left, right| {
        right
            .seconds
            .cmp(&left.seconds)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.category_id.cmp(&right.category_id))
    });
    let total: i64 = ranking.iter().map(|row| row.seconds).sum();
    for row in &mut ranking {
        row.share = if total > 0 {
            row.seconds as f64 / total as f64
        } else {
            0.0
        };
    }

    // ── trend ───────────────────────────────────────────────────────────────
    let productive = productive_by_date(&data.productive_seconds);
    let history = data.earliest_date.as_deref();
    let trend: Vec<TrendPoint> = each_date(&window.from, &window.to)
        .into_iter()
        .map(|date| TrendPoint {
            hours: hours_of(&productive, &date),
            ma30: moving_average(history, &productive, &date, 30),
            ma60: moving_average(history, &productive, &date, 60),
            date,
        })
        .collect();

    // ── heatmap ─────────────────────────────────────────────────────────────
    let heatmap = heatmap(&catalog, &data.hour_seconds, &window);

    // ── quality ─────────────────────────────────────────────────────────────
    let mut estimated: Vec<&RecordNightRow> = data
        .nights
        .iter()
        .filter(|night| {
            night.source != "paired"
                && night.wake_date.as_str() >= window.from.as_str()
                && night.wake_date.as_str() <= window.to.as_str()
        })
        .collect();
    // Oldest first; the load order (`wake_date, asleep_at`) breaks same-date
    // ties.
    estimated.sort_by(|left, right| left.wake_date.cmp(&right.wake_date));
    estimated.truncate(ESTIMATED_NIGHTS_CAP);
    let quality = RecordQuality {
        estimated_nights: estimated
            .into_iter()
            .map(|night| EstimatedNight {
                wake_date: night.wake_date.clone(),
                source: night.source.clone(),
                seconds: night.seconds,
            })
            .collect(),
        unclassified_titles: data
            .titles
            .iter()
            .take(UNCLASSIFIED_TITLES_CAP)
            .map(|row| UnclassifiedTitle {
                title: row.title.clone(),
                count: row.count,
            })
            .collect(),
    };

    // ── years ───────────────────────────────────────────────────────────────
    let as_of_year = parse_date(as_of).map(|date| date.year()).unwrap_or(1970);
    let years = (first_year(history, as_of)..=as_of_year)
        .map(|year| year_row(year, as_of, as_of_year, &productive, &data.nights))
        .collect();

    RecordReport {
        as_of: as_of.to_string(),
        time_zone: time_zone.to_string(),
        range: range.as_str().to_string(),
        from: window.from,
        to: window.to,
        previous_from: window.previous_from,
        previous_to: window.previous_to,
        days,
        ranking,
        trend,
        heatmap,
        quality,
        years,
    }
}

// ──────────────────────────────────────────
// The catalog the record renders from
// ──────────────────────────────────────────

/// The rules' one-level productive tree plus the colours `task_categories`
/// carries (which [`CategoryCatalog`] deliberately does not).
struct RecordCatalog {
    catalog: CategoryCatalog,
    colors: BTreeMap<String, String>,
}

impl RecordCatalog {
    fn new(rows: &[RecordCategoryRow]) -> Self {
        let colors = rows
            .iter()
            .map(|row| (row.id.clone(), row.color.clone()))
            .collect();
        let cats = rows
            .iter()
            .map(|row| {
                Cat::new(
                    &row.id,
                    row.parent_id.as_deref(),
                    row.is_productive,
                    &row.slug,
                    &row.title,
                )
            })
            .collect();
        Self {
            catalog: CategoryCatalog::new(cats),
            colors,
        }
    }

    /// The root a leaf rolls up to: its parent when it has one, else itself.
    /// `''` (unclassified) and `*sleep` are roots of their own.
    fn root(&self, id: &str) -> String {
        if id.is_empty() {
            return String::new();
        }
        if let Some(parent) = self
            .catalog
            .get(id)
            .and_then(|cat| cat.parent_id.as_deref())
            .filter(|parent| !parent.is_empty())
        {
            return parent.to_string();
        }
        id.to_string()
    }

    /// Whether a **leaf** id counts as productive: its own flag or its
    /// parent's, one level (the same rule the rules scorer uses).
    fn productive(&self, id: &str) -> bool {
        self.catalog.productive(id)
    }

    /// The title and colour a root id renders as.
    fn display(&self, id: &str) -> (String, String) {
        if id.is_empty() {
            return (
                UNCLASSIFIED_TITLE.to_string(),
                UNCLASSIFIED_COLOR.to_string(),
            );
        }
        if id == SYNTHETIC_SLEEP_CATEGORY_ID {
            return (SLEEP_TITLE.to_string(), SLEEP_COLOR.to_string());
        }
        let Some(cat) = self.catalog.get(id) else {
            return (UNKNOWN_TITLE.to_string(), UNKNOWN_COLOR.to_string());
        };
        let title = if cat.title.trim().is_empty() {
            UNKNOWN_TITLE.to_string()
        } else {
            cat.title.clone()
        };
        let color = self
            .colors
            .get(id)
            .map(|color| color.trim())
            .filter(|color| !color.is_empty())
            .unwrap_or(UNKNOWN_COLOR);
        (title, color.to_string())
    }
}

// ──────────────────────────────────────────
// Pieces
// ──────────────────────────────────────────

/// One day's root totals as the response's segment list.
fn segments_for(catalog: &RecordCatalog, totals: &BTreeMap<String, i64>) -> Vec<RecordSegment> {
    let mut segments: Vec<RecordSegment> = totals
        .iter()
        .filter(|(_, seconds)| **seconds > 0)
        .map(|(category_id, seconds)| {
            let (title, color) = catalog.display(category_id);
            RecordSegment {
                category_id: category_id.clone(),
                title,
                color,
                seconds: *seconds,
            }
        })
        .collect();
    segments.sort_by(|left, right| {
        right
            .seconds
            .cmp(&left.seconds)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.category_id.cmp(&right.category_id))
    });
    segments
}

/// Every civil date in `[from, to]`, oldest first.
fn each_date(from: &str, to: &str) -> Vec<String> {
    let mut dates = Vec::new();
    let mut cursor = from.to_string();
    loop {
        dates.push(cursor.clone());
        if cursor == to {
            break;
        }
        match shift_date(&cursor, 1) {
            Some(next) => cursor = next,
            None => break,
        }
    }
    dates
}

/// Per-day productive seconds, summed when the caller loaded overlapping
/// windows.
fn productive_by_date(rows: &[RecordDaySecondsRow]) -> BTreeMap<String, i64> {
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        *totals.entry(row.local_date.clone()).or_insert(0) += row.seconds;
    }
    totals
}

/// The day's productive hours (`0.0` when it has no productive row).
fn hours_of(productive: &BTreeMap<String, i64>, date: &str) -> f64 {
    productive.get(date).copied().unwrap_or(0) as f64 / HOUR_SECONDS
}

/// The mean productive hours over the `window` days ending `date`, or `None`
/// until the user's history reaches back that far.
///
/// The window never shrinks: a day inside it with no row counts as 0.
fn moving_average(
    history: Option<&str>,
    productive: &BTreeMap<String, i64>,
    date: &str,
    window: i64,
) -> Option<f64> {
    let history = history?;
    if days_between(history, date)? < window - 1 {
        return None;
    }
    let mut cursor = shift_date(date, -(window - 1))?;
    let mut total = 0.0;
    for step in 0..window {
        if step > 0 {
            cursor = shift_date(&cursor, 1)?;
        }
        total += hours_of(productive, &cursor);
    }
    Some(total / window as f64)
}

/// The 7 × 24 mean productive seconds grid.
///
/// The denominator is every occurrence of that weekday in the selected range
/// (zero-filled days included), and only productive categories contribute —
/// the same leaves the trend counts.
fn heatmap(
    catalog: &RecordCatalog,
    rows: &[RecordHourSecondsRow],
    window: &RecordWindow,
) -> Vec<HeatmapRow> {
    let mut occurrences = [0_i64; 7];
    for date in each_date(&window.from, &window.to) {
        if let Some(weekday) = weekday_of(&date) {
            occurrences[weekday as usize] += 1;
        }
    }

    let mut sums = [[0_i64; 24]; 7];
    for row in rows {
        let date = row.local_date.as_str();
        if date < window.from.as_str() || date > window.to.as_str() {
            continue;
        }
        if !(0..24).contains(&row.hour) || !catalog.productive(&row.category_id) {
            continue;
        }
        let Some(weekday) = weekday_of(date) else {
            continue;
        };
        sums[weekday as usize][row.hour as usize] += row.seconds;
    }

    (0..7)
        .map(|weekday| HeatmapRow {
            weekday: weekday as u32,
            seconds: (0..24)
                .map(|hour| {
                    let days = occurrences[weekday];
                    if days > 0 {
                        sums[weekday][hour] / days
                    } else {
                        0
                    }
                })
                .collect(),
        })
        .collect()
}

/// One calendar year's row.
fn year_row(
    year: i32,
    as_of: &str,
    as_of_year: i32,
    productive: &BTreeMap<String, i64>,
    nights: &[RecordNightRow],
) -> YearRow {
    let start = format!("{year:04}-01-01");
    // No future days: the running year stops at `as_of`.
    let end = if year >= as_of_year {
        as_of.to_string()
    } else {
        format!("{year:04}-12-31")
    };

    let mut days = 0_i64;
    let mut total = 0.0;
    let mut weekday_sum = 0.0;
    let mut weekday_days = 0_i64;
    let mut weekend_sum = 0.0;
    let mut weekend_days = 0_i64;
    for date in each_date(&start, &end) {
        let hours = hours_of(productive, &date);
        days += 1;
        total += hours;
        match weekday_of(&date) {
            Some(weekday) if weekday < 5 => {
                weekday_sum += hours;
                weekday_days += 1;
            }
            Some(_) => {
                weekend_sum += hours;
                weekend_days += 1;
            }
            None => {}
        }
    }

    let mut paired: Vec<f64> = nights
        .iter()
        .filter(|night| night.source == "paired" && civil_year(&night.wake_date) == Some(year))
        .map(|night| night.seconds as f64 / HOUR_SECONDS)
        .collect();
    paired.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));

    YearRow {
        year,
        productive_hours_per_day: if days > 0 { total / days as f64 } else { 0.0 },
        weekday_hours: if weekday_days > 0 {
            weekday_sum / weekday_days as f64
        } else {
            0.0
        },
        weekend_hours: if weekend_days > 0 {
            weekend_sum / weekend_days as f64
        } else {
            0.0
        },
        sleep_median_hours: if paired.is_empty() {
            None
        } else {
            Some(median(&paired))
        },
        paired_nights: paired.len() as i64,
        days,
    }
}

/// Python `statistics.median`: the middle value, or the mean of the two middle
/// values for an even count. The same rule the rules scorer uses.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    let len = sorted.len();
    if len % 2 == 1 {
        sorted[len / 2]
    } else {
        (sorted[len / 2 - 1] + sorted[len / 2]) / 2.0
    }
}

/// `0 = Monday … 6 = Sunday`.
fn weekday_of(date: &str) -> Option<u32> {
    Some(parse_date(date)?.weekday().num_days_from_monday())
}

/// The calendar year of a civil date.
fn civil_year(date: &str) -> Option<i32> {
    Some(parse_date(date)?.year())
}
