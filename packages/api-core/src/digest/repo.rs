//! Digest persistence: the [`DigestRepo`] trait, D1-ready SQL, and the pure
//! dirty-date helpers.
//!
//! SQL is a single source of truth here (same pattern as
//! [`crate::repo::calendar`]) so the statements can be asserted in tests, the
//! worker's D1 repos and `packages/d1-sync`'s rusqlite repos share them, and
//! d1-sync can apply the real migrations and round-trip a day.
//!
//! Every statement stays under D1's 100-bound-parameter limit: chunked
//! builders assert their own limit at call time.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::Deserialize;

use crate::models::{de_d1_bool, de_empty_string, de_optional_empty_string};
use crate::repo::RepoError;
use crate::time::{civil_date_in_zone, rfc3339_to_unix_secs};

use super::{effective_zone_name, shift_date, DayDigest, DigestEvent};

// ──────────────────────────────────────────
// dirty_local_days
// ──────────────────────────────────────────

/// Marks days dirty. `enqueued_at` is refreshed on conflict so the newest
/// cause wins (the tick does not care *when*, only *that*).
///
/// Binds: user_id, local_date, enqueued_at (3 per row).
pub const DIGEST_MARK_DIRTY_SQL: &str = "
    INSERT INTO dirty_local_days (user_id, local_date, enqueued_at)
    VALUES (?, ?, ?)
    ON CONFLICT(user_id, local_date) DO UPDATE SET enqueued_at = excluded.enqueued_at
";

/// Rows per dirty-mark statement (3 binds each; 33 rows = 99 binds).
pub const DIGEST_MARK_DIRTY_COL_COUNT: usize = 3;
pub const DIGEST_MARK_DIRTY_CHUNK_SIZE: usize = 100 / DIGEST_MARK_DIRTY_COL_COUNT; // 33

/// A day that may be touched by an interval, plus its ±1 calendar-day sleep
/// neighbours.
///
/// **Deliberate improvement over `calendar-ipynb`:** the civil dates come from
/// the event's **start IANA zone** (chrono-tz), not from the numeric offset
/// embedded in the RFC 3339 string. `end_tz` is consulted only when the start
/// zone is empty/unknown, mirroring the digest's dating rule.
///
/// Invalid timestamps yield no dates (the caller must not fail the write over
/// a bad dirty mark). The span is capped at [`DIRTY_SPAN_MAX_DAYS`] days: a
/// malformed/huge interval must not turn one write into thousands of rows.
pub fn dirty_dates_for_span(
    start_rfc3339: &str,
    end_rfc3339: &str,
    start_tz: &str,
    end_tz: &str,
) -> Vec<String> {
    let (Some(start), Some(end)) = (
        rfc3339_to_unix_secs(start_rfc3339),
        rfc3339_to_unix_secs(end_rfc3339),
    ) else {
        return Vec::new();
    };
    let zone = effective_zone_name(start_tz, end_tz);
    let first = civil_date_in_zone(start, &zone);
    // The interval is half-open, so `end` itself belongs to the next date only
    // when it is not exactly on a boundary; stepping back one second is the
    // cheap, correct way to name the last date the interval actually touches.
    let last_instant = if end > start { end - 1 } else { start };
    let last = civil_date_in_zone(last_instant, &zone);

    let mut dates: Vec<String> = Vec::new();
    let mut cursor = first;
    loop {
        dates.push(cursor.clone());
        if cursor == last || dates.len() >= DIRTY_SPAN_MAX_DAYS {
            break;
        }
        match shift_date(&cursor, 1) {
            Some(next) => cursor = next,
            None => break,
        }
    }

    let mut out: BTreeSet<String> = BTreeSet::new();
    for date in dates {
        if let Some(previous) = shift_date(&date, -1) {
            out.insert(previous);
        }
        out.insert(date.clone());
        if let Some(next) = shift_date(&date, 1) {
            out.insert(next);
        }
    }
    out.into_iter().collect()
}

/// Cap on one interval's dirty span (see [`dirty_dates_for_span`]).
pub const DIRTY_SPAN_MAX_DAYS: usize = 400;

/// One event span, as dirty marking sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpan<'a> {
    pub start_rfc3339: &'a str,
    pub end_rfc3339: &'a str,
    pub start_time_zone: &'a str,
    pub end_time_zone: &'a str,
}

/// Union of [`dirty_dates_for_span`] over several spans (an upsert unions the
/// stored span with the incoming one), sorted and deduplicated.
pub fn dirty_dates_for_spans(spans: &[EventSpan<'_>]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for span in spans {
        out.extend(dirty_dates_for_span(
            span.start_rfc3339,
            span.end_rfc3339,
            span.start_time_zone,
            span.end_time_zone,
        ));
    }
    out.into_iter().collect()
}

/// Builds one chunk of the dirty-mark upsert (`args` are the raw bound values
/// in statement order; INTEGER affinity coerces the timestamps it needs to).
pub fn build_mark_dirty_sql(
    user_id: &str,
    dates: &[String],
    now_rfc3339: &str,
) -> (String, Vec<String>) {
    assert!(
        !dates.is_empty(),
        "mark_dirty chunk must not be empty"
    );
    assert!(
        dates.len() <= DIGEST_MARK_DIRTY_CHUNK_SIZE,
        "mark_dirty chunk exceeds {DIGEST_MARK_DIRTY_CHUNK_SIZE} rows"
    );
    let mut sql = String::from(
        "INSERT INTO dirty_local_days (user_id, local_date, enqueued_at) VALUES ",
    );
    let mut args: Vec<String> = Vec::with_capacity(dates.len() * DIGEST_MARK_DIRTY_COL_COUNT);
    for (index, date) in dates.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?)");
        args.push(user_id.to_string());
        args.push(date.clone());
        args.push(now_rfc3339.to_string());
    }
    sql.push_str(" ON CONFLICT(user_id, local_date) DO UPDATE SET enqueued_at = excluded.enqueued_at");
    (sql, args)
}

/// The next dirty days for one user, oldest first.
pub const DIGEST_LIST_DIRTY_SQL: &str = "
    SELECT local_date FROM dirty_local_days
    WHERE user_id = ?
    ORDER BY local_date
    LIMIT ?
";

/// How many days are waiting to be rebuilt.
pub const DIGEST_COUNT_DIRTY_SQL: &str =
    "SELECT COUNT(*) AS dirty_days FROM dirty_local_days WHERE user_id = ?";

/// Clears one rebuilt day (idempotent).
pub const DIGEST_CLEAR_DIRTY_SQL: &str =
    "DELETE FROM dirty_local_days WHERE user_id = ? AND local_date = ?";

/// Users with work: a dirty day, or a job still `running` (so a stuck job is
/// always picked up again).
pub const DIGEST_USERS_WITH_WORK_SQL: &str = "
    SELECT user_id FROM dirty_local_days
    UNION
    SELECT user_id FROM report_job_status WHERE status = 'running'
    LIMIT ?
";

// ──────────────────────────────────────────
// daily_meta / daily_category_minutes / daily_hour_seconds / sleep_nights
// ──────────────────────────────────────────

/// Replaces the day's meta row. Binds: user_id, local_date, time_zone,
/// tracked_seconds, sleep_seconds, algo_version, computed_at (7).
pub const DIGEST_UPSERT_META_SQL: &str = "
    INSERT INTO daily_meta (
      user_id, local_date, time_zone, tracked_seconds, sleep_seconds,
      algo_version, computed_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(user_id, local_date) DO UPDATE SET
      time_zone = excluded.time_zone,
      tracked_seconds = excluded.tracked_seconds,
      sleep_seconds = excluded.sleep_seconds,
      algo_version = excluded.algo_version,
      computed_at = excluded.computed_at
";

/// One rebuilt day drops its category rows before re-inserting them.
pub const DIGEST_DELETE_CATEGORY_SQL: &str =
    "DELETE FROM daily_category_minutes WHERE user_id = ? AND local_date = ?";

/// One rebuilt day drops its hour rows before re-inserting them.
pub const DIGEST_DELETE_HOURS_SQL: &str =
    "DELETE FROM daily_hour_seconds WHERE user_id = ? AND local_date = ?";

/// One rebuilt day drops the nights that **wake** on it. A night belongs to
/// its `wake_date`; its pre-midnight synthetic seconds live in the previous
/// date's rows instead.
pub const DIGEST_DELETE_NIGHTS_SQL: &str =
    "DELETE FROM sleep_nights WHERE user_id = ? AND wake_date = ?";

/// Binds: user_id, local_date, category_id, seconds (4 per row).
pub const DIGEST_CATEGORY_INSERT_COL_COUNT: usize = 4;
pub const DIGEST_CATEGORY_INSERT_CHUNK_SIZE: usize = 100 / DIGEST_CATEGORY_INSERT_COL_COUNT; // 25

/// Binds: user_id, local_date, hour, category_id, seconds (5 per row).
pub const DIGEST_HOUR_INSERT_COL_COUNT: usize = 5;
pub const DIGEST_HOUR_INSERT_CHUNK_SIZE: usize = 100 / DIGEST_HOUR_INSERT_COL_COUNT; // 20

/// Binds: user_id, wake_date, asleep_at, woke_at, seconds,
/// bed_min_past_noon, wake_min_past_midnight, source (8 per row).
pub const DIGEST_NIGHT_INSERT_COL_COUNT: usize = 8;
pub const DIGEST_NIGHT_INSERT_CHUNK_SIZE: usize = 100 / DIGEST_NIGHT_INSERT_COL_COUNT; // 12

/// Builds one chunk of the category-seconds insert.
pub fn build_digest_category_insert_sql(
    user_id: &str,
    local_date: &str,
    rows: &[(String, i64)],
) -> (String, Vec<String>) {
    assert!(!rows.is_empty(), "category insert chunk must not be empty");
    assert!(
        rows.len() <= DIGEST_CATEGORY_INSERT_CHUNK_SIZE,
        "category insert chunk exceeds {DIGEST_CATEGORY_INSERT_CHUNK_SIZE} rows"
    );
    let mut sql = String::from(
        "INSERT INTO daily_category_minutes (user_id, local_date, category_id, seconds) VALUES ",
    );
    let mut args: Vec<String> = Vec::with_capacity(rows.len() * DIGEST_CATEGORY_INSERT_COL_COUNT);
    for (index, (category_id, seconds)) in rows.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?)");
        args.push(user_id.to_string());
        args.push(local_date.to_string());
        args.push(category_id.clone());
        args.push(seconds.to_string());
    }
    (sql, args)
}

/// Builds one chunk of the hour-seconds insert.
pub fn build_digest_hour_insert_sql(
    user_id: &str,
    local_date: &str,
    rows: &[(i64, String, i64)],
) -> (String, Vec<String>) {
    assert!(!rows.is_empty(), "hour insert chunk must not be empty");
    assert!(
        rows.len() <= DIGEST_HOUR_INSERT_CHUNK_SIZE,
        "hour insert chunk exceeds {DIGEST_HOUR_INSERT_CHUNK_SIZE} rows"
    );
    let mut sql = String::from(
        "INSERT INTO daily_hour_seconds (user_id, local_date, hour, category_id, seconds) VALUES ",
    );
    let mut args: Vec<String> = Vec::with_capacity(rows.len() * DIGEST_HOUR_INSERT_COL_COUNT);
    for (index, (hour, category_id, seconds)) in rows.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?,?)");
        args.push(user_id.to_string());
        args.push(local_date.to_string());
        args.push(hour.to_string());
        args.push(category_id.clone());
        args.push(seconds.to_string());
    }
    (sql, args)
}

/// Builds one chunk of the night insert. `args` binds the pre-rendered
/// RFC 3339 `Z` instants.
pub fn build_digest_night_insert_sql(
    user_id: &str,
    rows: &[NightInsert],
) -> (String, Vec<String>) {
    assert!(!rows.is_empty(), "night insert chunk must not be empty");
    assert!(
        rows.len() <= DIGEST_NIGHT_INSERT_CHUNK_SIZE,
        "night insert chunk exceeds {DIGEST_NIGHT_INSERT_CHUNK_SIZE} rows"
    );
    let mut sql = String::from(
        "INSERT OR REPLACE INTO sleep_nights (
           user_id, wake_date, asleep_at, woke_at, seconds,
           bed_min_past_noon, wake_min_past_midnight, source
         ) VALUES ",
    );
    let mut args: Vec<String> = Vec::with_capacity(rows.len() * DIGEST_NIGHT_INSERT_COL_COUNT);
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?,?,?,?,?)");
        args.push(user_id.to_string());
        args.push(row.wake_date.clone());
        args.push(row.asleep_at.clone());
        args.push(row.woke_at.clone());
        args.push(row.seconds.to_string());
        args.push(row.bed_min_past_noon.to_string());
        args.push(row.wake_min_past_midnight.to_string());
        args.push(row.source.to_string());
    }
    (sql, args)
}

/// One night row, ready to bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NightInsert {
    pub wake_date: String,
    pub asleep_at: String,
    pub woke_at: String,
    pub seconds: i64,
    pub bed_min_past_noon: i64,
    pub wake_min_past_midnight: i64,
    pub source: String,
}

// ──────────────────────────────────────────
// Source reads
// ──────────────────────────────────────────

/// Loads every cached event that *may* touch the inclusive civil-date window.
///
/// The window is **padded by the caller** (±2 days): `substr(start_time,1,10)`
/// compares RFC 3339 text lexicographically, which is not a true instant
/// compare across offsets, and a zone-shifted instant can land one civil date
/// away from its UTC stamp. Two days of slack is the documented compromise.
///
/// Deliberately unfiltered: soft-deleted rows, masters, all-day rows and
/// non-self-organized rows all come back — the pure pipeline drops them, so
/// the SQL stays a coarse index scan and the filter lives in one place.
pub const DIGEST_LOAD_EVENTS_SQL: &str = "
    SELECT
      e.id AS id,
      e.title AS title,
      e.start_time AS start_rfc3339,
      e.end_time AS end_rfc3339,
      e.start_time_zone AS start_time_zone,
      e.end_time_zone AS end_time_zone,
      e.category_id AS category_id,
      e.is_all_day AS is_all_day,
      e.is_self_organized AS is_self_organized,
      e.event_type AS event_type,
      e.recurrence AS recurrence,
      e.status AS status,
      e.deleted_at AS deleted_at
    FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ?
      AND c.deleted_at IS NULL
      AND substr(e.start_time, 1, 10) <= ?
      AND substr(e.end_time, 1, 10) >= ?
";

/// One `calendar_events` row, as the digest reads it.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestEventRow {
    pub id: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub title: String,
    pub start_rfc3339: String,
    pub end_rfc3339: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub start_time_zone: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub end_time_zone: String,
    #[serde(default, deserialize_with = "de_optional_empty_string")]
    pub category_id: Option<String>,
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub is_all_day: bool,
    #[serde(default = "default_true", deserialize_with = "de_d1_bool")]
    pub is_self_organized: bool,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub event_type: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub recurrence: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub status: String,
    pub deleted_at: Option<String>,
}

fn default_true() -> bool {
    true
}

impl From<DigestEventRow> for DigestEvent {
    fn from(row: DigestEventRow) -> Self {
        Self {
            id: row.id,
            title: row.title,
            start_rfc3339: row.start_rfc3339,
            end_rfc3339: row.end_rfc3339,
            start_time_zone: row.start_time_zone,
            end_time_zone: row.end_time_zone,
            category_id: row.category_id.filter(|id| !id.is_empty()),
            is_all_day: row.is_all_day,
            is_self_organized: row.is_self_organized,
            event_type: row.event_type,
            recurrence: row.recurrence,
            status: row.status,
            deleted: row.deleted_at.is_some_and(|value| !value.is_empty()),
        }
    }
}

/// The user's primary calendar zone — the digest's `fallback_tz`.
pub const DIGEST_PRIMARY_TIME_ZONE_SQL: &str = "
    SELECT time_zone FROM google_calendars
    WHERE user_id = ? AND is_primary = 1 AND deleted_at IS NULL
    ORDER BY created_at
    LIMIT 1
";

/// The earliest civil date any living event starts on — the backfill floor.
/// `None` (no row) also answers "does this user have any calendar event?".
pub const DIGEST_EARLIEST_EVENT_DATE_SQL: &str = "
    SELECT MIN(substr(e.start_time, 1, 10)) AS earliest_date
    FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ?
      AND c.deleted_at IS NULL
      AND e.deleted_at IS NULL
";

// ──────────────────────────────────────────
// report_job_status
// ──────────────────────────────────────────

pub const DIGEST_JOB_STATUS_SQL: &str =
    "SELECT status, cursor FROM report_job_status WHERE user_id = ? AND kind = ?";

/// Binds: user_id, kind, status, cursor, updated_at.
pub const DIGEST_UPSERT_JOB_STATUS_SQL: &str = "
    INSERT INTO report_job_status (user_id, kind, status, cursor, updated_at)
    VALUES (?, ?, ?, ?, ?)
    ON CONFLICT(user_id, kind) DO UPDATE SET
      status = excluded.status,
      cursor = excluded.cursor,
      updated_at = excluded.updated_at
";

/// Has this user ever been computed?
pub const DIGEST_HAS_META_SQL: &str = "
    SELECT 1 AS present FROM daily_meta WHERE user_id = ? LIMIT 1
";

/// Any row computed by a different [`super::ALGO_VERSION`]?
pub const DIGEST_HAS_STALE_META_SQL: &str = "
    SELECT 1 AS present FROM daily_meta WHERE user_id = ? AND algo_version != ? LIMIT 1
";

// ──────────────────────────────────────────
// Reclassify
// ──────────────────────────────────────────

/// One reclassify batch: every living event of the user whose id sorts after
/// the cursor, oldest id first.
///
/// `category_id` is read as well as written: the edge case "the id did not
/// change ⇒ no dirty mark" needs the stored stamp, and re-writing an identical
/// stamp would only churn.
pub const DIGEST_RECLASSIFY_BATCH_SQL: &str = "
    SELECT
      e.id AS id,
      e.title AS title,
      c.google_calendar_id AS google_calendar_id,
      e.start_time AS start_time,
      e.end_time AS end_time,
      e.start_time_zone AS start_time_zone,
      e.end_time_zone AS end_time_zone,
      e.category_id AS category_id
    FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ?
      AND c.deleted_at IS NULL
      AND e.deleted_at IS NULL
      AND e.id > ?
    ORDER BY e.id
    LIMIT ?
";

/// One row of a reclassify batch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReclassifyRow {
    pub id: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub title: String,
    /// **Google** calendar id — the scope for pattern matching.
    pub google_calendar_id: String,
    pub start_time: String,
    pub end_time: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub start_time_zone: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub end_time_zone: String,
    #[serde(default, deserialize_with = "de_optional_empty_string")]
    pub category_id: Option<String>,
}

/// Writes the classifier's verdict. `NULLIF(?, '')` turns an empty string into
/// SQL `NULL`, so a `None` (unclassified) clears a stale stamp. Deliberately
/// does NOT touch `updated_at`: a re-stamp is not a content change.
pub const DIGEST_SET_CATEGORY_SQL: &str =
    "UPDATE calendar_events SET category_id = NULLIF(?, '') WHERE id = ? AND deleted_at IS NULL";

/// The stored span of one event, for dirty marking an edit/delete. No
/// `deleted_at` filter — a resurrected row's old span still matters.
pub const DIGEST_EVENT_SPAN_SQL: &str = "
    SELECT start_time, end_time, start_time_zone, end_time_zone
    FROM calendar_events
    WHERE calendar_id = ? AND google_event_id = ?
";

/// The owner of a calendar, for dirty marking. A missing/soft-deleted calendar
/// skips the mark (the event write must never fail because of it).
pub const DIGEST_CALENDAR_OWNER_SQL: &str =
    "SELECT user_id FROM google_calendars WHERE id = ? AND deleted_at IS NULL";

/// One stored event span.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EventSpanRow {
    pub start_time: String,
    pub end_time: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub start_time_zone: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub end_time_zone: String,
}

impl EventSpanRow {
    /// Borrows the row as an [`EventSpan`].
    pub fn as_span(&self) -> EventSpan<'_> {
        EventSpan {
            start_rfc3339: &self.start_time,
            end_rfc3339: &self.end_time,
            start_time_zone: &self.start_time_zone,
            end_time_zone: &self.end_time_zone,
        }
    }
}

/// A `report_job_status` row.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JobStatus {
    pub status: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub cursor: String,
}

impl JobStatus {
    /// `status == "running"`.
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

// ──────────────────────────────────────────
// The trait
// ──────────────────────────────────────────

/// Persistence for the digest tables.
///
/// Two implementations exist: `apps/worker/src/db/digest.rs` (`D1DigestRepo`,
/// production) and `packages/d1-sync/src/repos.rs` (`SqliteDigestRepo`, which
/// applies the real migrations and round-trips a day in `cargo test -p
/// d1-sync`).
///
/// `replace_day` is a **logical** replace, not a transaction: the DELETEs and
/// INSERTs run in sequence, and a crash midway leaves a partial day that the
/// next dirty rebuild repairs. Idempotence matters more than atomicity here —
/// running the same day twice must leave identical rows.
#[async_trait(?Send)]
pub trait DigestRepo: Send + Sync {
    /// Upserts dirty days (`enqueued_at` refreshed). Empty `dates` is a no-op.
    async fn mark_dirty(
        &self,
        user_id: &str,
        dates: &[String],
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// The next dirty days to rebuild, oldest first.
    async fn list_dirty(&self, user_id: &str, limit: usize) -> Result<Vec<String>, RepoError>;
    /// How many days are waiting to be rebuilt.
    async fn count_dirty(&self, user_id: &str) -> Result<i64, RepoError>;
    /// Drops dirty rows for the days a caller just rebuilt. Empty `dates` is a
    /// no-op.
    async fn clear_dirty(&self, user_id: &str, dates: &[String]) -> Result<(), RepoError>;
    /// Users with a dirty day or a `running` job, at most `limit`.
    async fn users_with_work(&self, limit: usize) -> Result<Vec<String>, RepoError>;
    /// Replaces one day's meta + category + hour rows and its `wake_date`
    /// nights. Idempotent.
    async fn replace_day(&self, user_id: &str, day: &DayDigest) -> Result<(), RepoError>;
    /// Every cached event that may touch the (already padded) civil-date
    /// window. See [`DIGEST_LOAD_EVENTS_SQL`].
    async fn load_events_touching(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<DigestEvent>, RepoError>;
    /// The user's primary calendar IANA zone, or `None` before the first sync.
    async fn primary_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError>;
    /// A job's status row, or `None` when it never ran.
    async fn job_status(&self, user_id: &str, kind: &str) -> Result<Option<JobStatus>, RepoError>;
    /// Upserts a job's status + cursor.
    async fn upsert_job_status(
        &self,
        user_id: &str,
        kind: &str,
        status: &str,
        cursor: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// The next `limit` living events after `after_id`, ordered by id.
    async fn reclassify_batch(
        &self,
        user_id: &str,
        after_id: &str,
        limit: usize,
    ) -> Result<Vec<ReclassifyRow>, RepoError>;
    /// Rewrites one event's leaf stamp (`None`/empty clears it to `NULL`).
    async fn set_category(&self, id: &str, category_id: Option<&str>) -> Result<(), RepoError>;
    /// Whether any `daily_meta` row exists for the user.
    async fn has_meta(&self, user_id: &str) -> Result<bool, RepoError>;
    /// Whether any `daily_meta` row was computed by a different `algo_version`.
    async fn has_stale_meta(&self, user_id: &str, algo_version: i64) -> Result<bool, RepoError>;
    /// The earliest civil date a living event starts on.
    async fn earliest_event_date(&self, user_id: &str) -> Result<Option<String>, RepoError>;
}
