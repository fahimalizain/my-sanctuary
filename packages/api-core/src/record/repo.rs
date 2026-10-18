//! Record persistence: the [`RecordRepo`] trait plus D1-ready SQL.
//!
//! SQL is the single source of truth here (same pattern as
//! [`crate::digest::repo`] and [`crate::rules::repo`]) so the statements can be
//! asserted in tests, the Worker's D1 repo and `packages/d1-sync`'s rusqlite
//! repo share them, and d1-sync can apply the real migrations and round-trip a
//! report.
//!
//! Every statement stays well under D1's 100-bound-parameter limit: these are
//! coarse range scans of one user's aggregates, never per-row writes.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::models::{de_d1_bool, de_empty_string, de_optional_empty_string};
use crate::repo::RepoError;

// ──────────────────────────────────────────
// SQL
// ──────────────────────────────────────────

/// The user's primary calendar zone. The same statement the digest reads
/// (`crate::digest::repo::DIGEST_PRIMARY_TIME_ZONE_SQL`) — re-exported here so
/// each record repo has one import surface.
pub const RECORD_PRIMARY_TIME_ZONE_SQL: &str = crate::digest::repo::DIGEST_PRIMARY_TIME_ZONE_SQL;

/// The user's living categories, colours included.
pub const RECORD_CATEGORIES_SQL: &str = "
    SELECT id, parent_id, title, color, slug, is_productive
    FROM task_categories
    WHERE user_id = ? AND deleted_at IS NULL
    ORDER BY sort_order, title
";

/// One day's meta row. A missing date is a zero day, so the caller never
/// invents one.
pub const RECORD_META_SQL: &str = "
    SELECT local_date, time_zone, tracked_seconds
    FROM daily_meta
    WHERE user_id = ? AND local_date >= ? AND local_date <= ?
    ORDER BY local_date
";

/// The leaf category seconds the segments and the ranking roll up. `''` =
/// unclassified, `*sleep` = synthetic sleep.
pub const RECORD_CATEGORY_SECONDS_SQL: &str = "
    SELECT local_date, category_id, seconds
    FROM daily_category_minutes
    WHERE user_id = ? AND local_date >= ? AND local_date <= ?
    ORDER BY local_date, category_id
";

/// The hour buckets the heatmap averages.
pub const RECORD_HOUR_SECONDS_SQL: &str = "
    SELECT local_date, hour, category_id, seconds
    FROM daily_hour_seconds
    WHERE user_id = ? AND local_date >= ? AND local_date <= ?
    ORDER BY local_date, hour, category_id
";

/// Per-day **productive** seconds, for the trend windows and the year rows.
///
/// The one-level productive rule is duplicated in SQL here (it cannot be
/// expressed through [`crate::rules::CategoryCatalog`] without shipping every
/// row): a leaf counts when its own `is_productive` is set, or its parent's is.
/// A `''` / `*sleep` row has no `task_categories` row and never counts.
pub const RECORD_PRODUCTIVE_SECONDS_SQL: &str = "
    SELECT m.local_date AS local_date, SUM(m.seconds) AS seconds
    FROM daily_category_minutes m
    LEFT JOIN task_categories c
      ON c.id = m.category_id AND c.user_id = m.user_id AND c.deleted_at IS NULL
    LEFT JOIN task_categories p
      ON p.id = c.parent_id AND p.user_id = m.user_id AND p.deleted_at IS NULL
    WHERE m.user_id = ? AND m.local_date >= ? AND m.local_date <= ?
      AND (c.is_productive = 1 OR p.is_productive = 1)
    GROUP BY m.local_date
    ORDER BY m.local_date
";

/// Every night that wakes in the window, paired and estimated alike.
pub const RECORD_NIGHTS_SQL: &str = "
    SELECT wake_date, source, seconds
    FROM sleep_nights
    WHERE user_id = ? AND wake_date >= ? AND wake_date <= ?
    ORDER BY wake_date, asleep_at
";

/// The earliest day any aggregate was computed for — the report's history
/// floor.
pub const RECORD_EARLIEST_META_DATE_SQL: &str =
    "SELECT MIN(local_date) AS earliest_date FROM daily_meta WHERE user_id = ?";

/// The earliest night's wake date (a user can have nights before their first
/// `daily_meta` row).
pub const RECORD_EARLIEST_NIGHT_DATE_SQL: &str =
    "SELECT MIN(wake_date) AS earliest_date FROM sleep_nights WHERE user_id = ?";

/// The candidate events `quality` flags: living, self-organized, non-recurring,
/// non-all-day events the classifier never stamped, with the instant and zone
/// the assembler needs to place each one on the user's own civil calendar.
///
/// `substr(start_time, 1, 10)` is the **UTC** date prefix of the RFC 3339
/// stamp, not the event's local civil date, so the caller pads the bounds by a
/// day on each side and the assembler keeps only the rows whose local date
/// falls inside the report's window. Grouping, ranking, and the cap all happen
/// there too.
pub const RECORD_UNCLASSIFIED_TITLES_SQL: &str = "
    SELECT e.title AS title, e.start_time AS start_time, e.start_time_zone AS start_time_zone
    FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ?
      AND c.deleted_at IS NULL
      AND e.deleted_at IS NULL
      AND e.status != 'cancelled'
      AND e.is_all_day = 0
      AND e.is_self_organized = 1
      AND (e.recurrence IS NULL OR e.recurrence = '')
      AND e.event_type IN ('', 'default', 'fromGmail')
      AND (e.category_id IS NULL OR e.category_id = '')
      AND substr(e.start_time, 1, 10) >= ?
      AND substr(e.start_time, 1, 10) <= ?
    ORDER BY e.title, e.start_time
";

// ──────────────────────────────────────────
// Rows
// ──────────────────────────────────────────

/// One living `task_categories` row, as the record renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordCategoryRow {
    pub id: String,
    #[serde(default, deserialize_with = "de_optional_empty_string")]
    pub parent_id: Option<String>,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub title: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub color: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub slug: String,
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub is_productive: bool,
}

/// One `daily_meta` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMetaRow {
    pub local_date: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub time_zone: String,
    pub tracked_seconds: i64,
}

/// One `daily_category_minutes` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordCategorySecondsRow {
    pub local_date: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub category_id: String,
    pub seconds: i64,
}

/// One `daily_hour_seconds` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordHourSecondsRow {
    pub local_date: String,
    pub hour: i64,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub category_id: String,
    pub seconds: i64,
}

/// One day's productive total (the [`RECORD_PRODUCTIVE_SECONDS_SQL`] aggregate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordDaySecondsRow {
    pub local_date: String,
    pub seconds: i64,
}

/// One `sleep_nights` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordNightRow {
    pub wake_date: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub source: String,
    pub seconds: i64,
}

/// One candidate event for the `quality` title list: the title plus the start
/// instant and zone the assembler resolves to a local civil date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordTitleCandidateRow {
    #[serde(default, deserialize_with = "de_empty_string")]
    pub title: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub start_time: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub start_time_zone: String,
}

// ──────────────────────────────────────────
// The trait
// ──────────────────────────────────────────

/// Persistence for the record report's reads.
///
/// Two implementations exist: `apps/worker/src/db/record.rs` (`D1RecordRepo`,
/// production) and `packages/d1-sync/src/record.rs` (`SqliteRecordRepo`, which
/// applies the real migrations and round-trips a report in `cargo test -p
/// d1-sync`).
///
/// Every method is a coarse inclusive-date-range read; the fold lives in
/// [`super::assemble`]. The ranges the service asks for are derived from the
/// report's own windows (see [`super::service`]).
#[async_trait(?Send)]
pub trait RecordRepo: Send + Sync {
    /// The user's primary calendar IANA zone, or `None` before the first sync.
    async fn primary_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError>;
    /// The user's living categories (one-level tree, colours included).
    async fn load_categories(&self, user_id: &str) -> Result<Vec<RecordCategoryRow>, RepoError>;
    /// `daily_meta` over the inclusive date range.
    async fn load_meta(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordMetaRow>, RepoError>;
    /// `daily_category_minutes` over the inclusive date range.
    async fn load_category_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordCategorySecondsRow>, RepoError>;
    /// `daily_hour_seconds` over the inclusive date range.
    async fn load_hour_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordHourSecondsRow>, RepoError>;
    /// Per-day productive seconds over the inclusive date range.
    async fn load_productive_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordDaySecondsRow>, RepoError>;
    /// `sleep_nights` (every source) over the inclusive wake-date range.
    async fn load_nights(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordNightRow>, RepoError>;
    /// The earliest `daily_meta.local_date`.
    async fn earliest_meta_date(&self, user_id: &str) -> Result<Option<String>, RepoError>;
    /// The earliest `sleep_nights.wake_date`.
    async fn earliest_night_date(&self, user_id: &str) -> Result<Option<String>, RepoError>;
    /// The candidate events for the `quality` title list: every living
    /// unclassified event whose **UTC date prefix** falls in the inclusive
    /// range. Callers pass a range padded by a day on each side; the assembler
    /// keeps only the rows whose local civil date is inside the report's own
    /// window, then groups and caps them.
    async fn unclassified_titles(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordTitleCandidateRow>, RepoError>;
}

// ──────────────────────────────────────────
// Small date helpers
// ──────────────────────────────────────────

/// The earliest of two optional civil dates.
pub fn earliest_of(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, right) => right,
    }
}

/// The earlier of two civil dates (both `YYYY-MM-DD`, so lexicographic order
/// is date order).
pub fn min_date(left: &str, right: &str) -> String {
    left.min(right).to_string()
}
