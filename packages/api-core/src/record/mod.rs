//! "The Record": descriptive reports over the daily aggregates (reports
//! slice 5).
//!
//! Nothing here scores, judges, or targets anything. The report answers what
//! the diary actually contained — the days, the category ranking, the hour ×
//! weekday shape, the year-by-year means — from the digest tables alone, and
//! leaves every verdict to [`crate::rules`].
//!
//! # Units
//!
//! Every duration in the stored aggregates is an integer number of **seconds**
//! (see [`crate::digest`]); the response keeps them that way and the client
//! divides by 3600 for hours. The only hours in this module are the derived
//! means the client would otherwise repeat (`trend`, `years`).
//!
//! # Layout
//!
//! * [`assemble`] — the pure assembler: loaded rows in, [`RecordReport`] out.
//! * [`repo`] — the [`repo::RecordRepo`] trait plus the D1-ready SQL.
//! * [`service`] — the async entry point the Worker calls.

pub mod assemble;
pub mod repo;
pub mod service;

#[cfg(test)]
mod tests;

pub use assemble::{
    assemble_record, EstimatedNight, HeatmapRow, RankingRow, RecordData, RecordDay, RecordQuality,
    RecordReport, RecordSegment, TrendPoint, UnclassifiedTitle, YearRow, ESTIMATED_NIGHTS_CAP,
    UNCLASSIFIED_TITLES_CAP, YEARS_CAP,
};
pub use repo::{
    RecordCategoryRow, RecordCategorySecondsRow, RecordDaySecondsRow, RecordHourSecondsRow,
    RecordMetaRow, RecordNightRow, RecordRepo, RecordTitleCandidateRow, RECORD_CATEGORIES_SQL,
    RECORD_CATEGORY_SECONDS_SQL, RECORD_EARLIEST_META_DATE_SQL, RECORD_EARLIEST_NIGHT_DATE_SQL,
    RECORD_HOUR_SECONDS_SQL, RECORD_META_SQL, RECORD_NIGHTS_SQL, RECORD_PRIMARY_TIME_ZONE_SQL,
    RECORD_PRODUCTIVE_SECONDS_SQL, RECORD_UNCLASSIFIED_TITLES_SQL,
};
pub use service::{record_report, TREND_WINDOW_DAYS};

use std::str::FromStr;

use chrono::Datelike;
use thiserror::Error;

use crate::digest::shift_date;
use crate::repo::RepoError;

/// Errors produced by the record service.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecordError {
    /// 400 — the caller asked for a range that does not exist.
    #[error("{0}")]
    Invalid(String),
    /// 500 — the database failed.
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

// ──────────────────────────────────────────
// Range
// ──────────────────────────────────────────

/// The `range` query values: `7d | 30d | 90d | ytd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordRange {
    Last7,
    #[default]
    Last30,
    Last90,
    YearToDate,
}

impl RecordRange {
    /// Every accepted value, in picker order.
    pub const ALL: [RecordRange; 4] = [
        RecordRange::Last7,
        RecordRange::Last30,
        RecordRange::Last90,
        RecordRange::YearToDate,
    ];

    /// The exact `range` string this value accepts and echoes back.
    pub fn as_str(self) -> &'static str {
        match self {
            RecordRange::Last7 => "7d",
            RecordRange::Last30 => "30d",
            RecordRange::Last90 => "90d",
            RecordRange::YearToDate => "ytd",
        }
    }

    /// Parses a `range` query value. Exact match only — anything else is a 400.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|range| range.as_str() == value)
    }

    /// The inclusive `[from, to]` civil-date window ending on `as_of`.
    pub fn dates(self, as_of: &str) -> Option<(String, String)> {
        let from = match self {
            RecordRange::Last7 => shift_date(as_of, -6)?,
            RecordRange::Last30 => shift_date(as_of, -29)?,
            RecordRange::Last90 => shift_date(as_of, -89)?,
            RecordRange::YearToDate => format!("{:04}-01-01", parse_date(as_of)?.year()),
        };
        Some((from, as_of.to_string()))
    }

    /// The selected window plus the previous window of the **same length**
    /// immediately before it.
    pub fn window(self, as_of: &str) -> Option<RecordWindow> {
        let (from, to) = self.dates(as_of)?;
        let length = days_between(&from, &to)? + 1;
        let previous_to = shift_date(&from, -1)?;
        let previous_from = shift_date(&previous_to, -(length - 1))?;
        Some(RecordWindow {
            from,
            to,
            previous_from,
            previous_to,
        })
    }
}

/// The selected window and the previous window it is compared against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordWindow {
    pub from: String,
    pub to: String,
    pub previous_from: String,
    pub previous_to: String,
}

impl RecordWindow {
    /// The number of civil days in the window (`to - from + 1`).
    pub fn length(&self) -> i64 {
        days_between(&self.from, &self.to).unwrap_or(0) + 1
    }
}

// ──────────────────────────────────────────
// Shared date helpers
// ──────────────────────────────────────────

/// Parses a `YYYY-MM-DD` civil date.
pub(crate) fn parse_date(date: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::from_str(date).ok()
}

/// Whole days from `from` to `to` (negative when `to` is the earlier date).
pub(crate) fn days_between(from: &str, to: &str) -> Option<i64> {
    Some((parse_date(to)? - parse_date(from)?).num_days())
}

/// The first calendar year the report shows: the year of `earliest` (the
/// user's first aggregate day), capped to the [`YEARS_CAP`] most recent years
/// ending at `as_of`'s year.
///
/// With no history at all the span is just `as_of`'s own year, so the table
/// renders one (all-zero) row instead of nothing.
pub(crate) fn first_year(earliest: Option<&str>, as_of: &str) -> i32 {
    let as_of_year = parse_date(as_of).map(|date| date.year()).unwrap_or(1970);
    let start = earliest
        .and_then(parse_date)
        .map(|date| date.year())
        .unwrap_or(as_of_year);
    start.max(as_of_year - (YEARS_CAP - 1))
}

/// Local midnight of January 1 of [`first_year`], as a civil date.
pub(crate) fn first_year_start(earliest: Option<&str>, as_of: &str) -> String {
    format!("{:04}-01-01", first_year(earliest, as_of))
}
