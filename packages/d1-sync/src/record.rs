//! rusqlite implementation of the record repo trait.
//!
//! Bind order mirrors `apps/worker/src/db/record.rs`; SQL constants come from
//! `api_core::record`. The point of this impl is that `cargo test -p d1-sync`
//! can apply the real Worker migrations and round-trip a report through the
//! real schema (affinity, the productive join, the unclassified-title filters)
//! without D1.

use api_core::record::{
    RecordCategoryRow, RecordCategorySecondsRow, RecordDaySecondsRow, RecordHourSecondsRow,
    RecordMetaRow, RecordNightRow, RecordRepo, RecordTitleCandidateRow, RECORD_CATEGORIES_SQL,
    RECORD_CATEGORY_SECONDS_SQL, RECORD_EARLIEST_META_DATE_SQL, RECORD_EARLIEST_NIGHT_DATE_SQL,
    RECORD_HOUR_SECONDS_SQL, RECORD_META_SQL, RECORD_NIGHTS_SQL, RECORD_PRIMARY_TIME_ZONE_SQL,
    RECORD_PRODUCTIVE_SECONDS_SQL, RECORD_UNCLASSIFIED_TITLES_SQL,
};
use api_core::repo::RepoError;
use rusqlite::Connection;
use serde::Deserialize;

use crate::repos::lock;
use crate::{query_one, query_vec, Db};

/// The reads behind `GET /api/reports/record`, over one rusqlite connection.
pub struct SqliteRecordRepo {
    db: Db,
}

impl SqliteRecordRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// A `SELECT MIN(...)` row: no matching rows still yields one row with NULL.
#[derive(Debug, Deserialize)]
struct EarliestRow {
    earliest_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZoneRow {
    time_zone: String,
}

#[async_trait::async_trait(?Send)]
impl RecordRepo for SqliteRecordRepo {
    async fn primary_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        let conn = lock(&self.db)?;
        let row: Option<ZoneRow> = query_one(&conn, RECORD_PRIMARY_TIME_ZONE_SQL, &[&user_id])?;
        Ok(row.map(|row| row.time_zone))
    }

    async fn load_categories(&self, user_id: &str) -> Result<Vec<RecordCategoryRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, RECORD_CATEGORIES_SQL, &[&user_id])
    }

    async fn load_meta(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordMetaRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, RECORD_META_SQL, &[&user_id, &from_date, &to_date])
    }

    async fn load_category_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordCategorySecondsRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            RECORD_CATEGORY_SECONDS_SQL,
            &[&user_id, &from_date, &to_date],
        )
    }

    async fn load_hour_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordHourSecondsRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            RECORD_HOUR_SECONDS_SQL,
            &[&user_id, &from_date, &to_date],
        )
    }

    async fn load_productive_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordDaySecondsRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            RECORD_PRODUCTIVE_SECONDS_SQL,
            &[&user_id, &from_date, &to_date],
        )
    }

    async fn load_nights(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordNightRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, RECORD_NIGHTS_SQL, &[&user_id, &from_date, &to_date])
    }

    async fn earliest_meta_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        let conn = lock(&self.db)?;
        let row: Option<EarliestRow> =
            query_one(&conn, RECORD_EARLIEST_META_DATE_SQL, &[&user_id])?;
        Ok(row.and_then(|row| row.earliest_date))
    }

    async fn earliest_night_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        let conn = lock(&self.db)?;
        let row: Option<EarliestRow> =
            query_one(&conn, RECORD_EARLIEST_NIGHT_DATE_SQL, &[&user_id])?;
        Ok(row.and_then(|row| row.earliest_date))
    }

    async fn unclassified_titles(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordTitleCandidateRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            RECORD_UNCLASSIFIED_TITLES_SQL,
            &[&user_id, &from_date, &to_date],
        )
    }
}

/// Seeds one living category with a parent and a colour (test helper). The
/// `seed_category` in `rules.rs` is the parentless, `#000000` variant the rule
/// tests use.
#[allow(clippy::too_many_arguments)]
pub fn seed_category_full(
    conn: &Connection,
    user_id: &str,
    id: &str,
    parent_id: Option<&str>,
    slug: &str,
    title: &str,
    is_productive: bool,
    color: &str,
) -> Result<(), RepoError> {
    crate::exec(
        conn,
        "INSERT INTO task_categories (
           id, user_id, list_id, parent_id, title, slug, color, is_productive,
           google_calendar_id, sort_order, is_untracked, created_at, updated_at
         ) VALUES (?, ?, NULL, ?, ?, ?, ?, ?, NULL, 0, 0, ?, ?)",
        &[
            &id,
            &user_id,
            &parent_id,
            &title,
            &slug,
            &color,
            &i64::from(is_productive),
            &"2026-01-01T00:00:00Z",
            &"2026-01-01T00:00:00Z",
        ],
    )
}

/// Seeds one day of `daily_meta` (test helper).
pub fn seed_meta(
    conn: &Connection,
    user_id: &str,
    local_date: &str,
    time_zone: &str,
    tracked_seconds: i64,
    sleep_seconds: i64,
) -> Result<(), RepoError> {
    crate::exec(
        conn,
        "INSERT INTO daily_meta (
           user_id, local_date, time_zone, tracked_seconds, sleep_seconds,
           algo_version, computed_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(user_id, local_date) DO UPDATE SET
           time_zone = excluded.time_zone,
           tracked_seconds = excluded.tracked_seconds,
           sleep_seconds = excluded.sleep_seconds",
        &[
            &user_id,
            &local_date,
            &time_zone,
            &tracked_seconds,
            &sleep_seconds,
            &1_i64,
            &"2026-01-01T00:00:00Z",
        ],
    )
}

/// Seeds one hour bucket (test helper).
pub fn seed_hour_seconds(
    conn: &Connection,
    user_id: &str,
    local_date: &str,
    hour: i64,
    category_id: &str,
    seconds: i64,
) -> Result<(), RepoError> {
    crate::exec(
        conn,
        "INSERT INTO daily_hour_seconds (user_id, local_date, hour, category_id, seconds)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(user_id, local_date, hour, category_id) DO UPDATE SET seconds = excluded.seconds",
        &[&user_id, &local_date, &hour, &category_id, &seconds],
    )
}

/// Seeds one sleep night (test helper).
pub fn seed_night(
    conn: &Connection,
    user_id: &str,
    wake_date: &str,
    asleep_at: &str,
    seconds: i64,
    source: &str,
) -> Result<(), RepoError> {
    crate::exec(
        conn,
        "INSERT INTO sleep_nights (
           user_id, wake_date, asleep_at, woke_at, seconds,
           bed_min_past_noon, wake_min_past_midnight, source
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(user_id, asleep_at) DO UPDATE SET
           wake_date = excluded.wake_date,
           seconds = excluded.seconds,
           source = excluded.source",
        &[
            &user_id, &wake_date, &asleep_at, &wake_date, &seconds, &0_i64, &0_i64, &source,
        ],
    )
}

/// Seeds one calendar event (test helper) — enough columns for the
/// unclassified-title query. `start_time_zone` is the event's own IANA zone
/// (`''` = none), which is what the record's title filter reads.
#[allow(clippy::too_many_arguments)]
pub fn seed_event(
    conn: &Connection,
    id: &str,
    calendar_id: &str,
    title: &str,
    start_time: &str,
    start_time_zone: &str,
    status: &str,
    is_all_day: bool,
    is_self_organized: bool,
    event_type: &str,
    recurrence: Option<&str>,
    category_id: Option<&str>,
) -> Result<(), RepoError> {
    crate::exec(
        conn,
        "INSERT INTO calendar_events (
           id, calendar_id, google_event_id, last_synced_at, title, start_time,
           start_time_zone, end_time, status, is_all_day, is_self_organized,
           event_type, recurrence, category_id, deleted_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        &[
            &id,
            &calendar_id,
            &id,
            &"2026-01-01T00:00:00Z",
            &title,
            &start_time,
            &start_time_zone,
            &start_time,
            &status,
            &i64::from(is_all_day),
            &i64::from(is_self_organized),
            &event_type,
            &recurrence,
            &category_id,
        ],
    )
}
