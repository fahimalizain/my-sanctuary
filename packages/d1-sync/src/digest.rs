//! rusqlite implementations of the digest repo traits.
//!
//! Bind order mirrors `apps/worker/src/db/digest.rs`; SQL constants come from
//! `api_core::digest::repo`. The point of this impl is that
//! `cargo test -p d1-sync` can apply the real Worker migrations and round-trip
//! a day, so production SQL (affinity, primary keys, `NULLIF`) is exercised
//! without D1.

use api_core::digest::repo::{
    build_digest_category_insert_sql, build_digest_hour_insert_sql,
    build_digest_night_insert_sql, build_mark_dirty_sql, DigestRepo, EventSpan, EventSpanRow,
    JobStatus, ReclassifyRow, NightInsert, DIGEST_CATEGORY_INSERT_CHUNK_SIZE,
    DIGEST_CLEAR_DIRTY_SQL, DIGEST_COUNT_DIRTY_SQL, DIGEST_DELETE_CATEGORY_SQL,
    DIGEST_DELETE_HOURS_SQL, DIGEST_DELETE_NIGHTS_SQL, DIGEST_EARLIEST_EVENT_DATE_SQL,
    DIGEST_EVENT_SPAN_SQL, DIGEST_HAS_META_SQL, DIGEST_HAS_STALE_META_SQL, DIGEST_JOB_STATUS_SQL,
    DIGEST_LATEST_ZONE_BEFORE_SQL, DIGEST_LIST_DIRTY_SQL, DIGEST_LOAD_EVENTS_SQL,
    DIGEST_MARK_DIRTY_CHUNK_SIZE, DIGEST_NIGHT_INSERT_CHUNK_SIZE, DIGEST_PRIMARY_TIME_ZONE_SQL,
    DIGEST_RECLASSIFY_BATCH_SQL, DIGEST_SET_CATEGORY_SQL, DIGEST_UPSERT_JOB_STATUS_SQL,
    DIGEST_UPSERT_META_SQL, DIGEST_USERS_WITH_WORK_SQL,
};
use api_core::digest::DayDigest;
use api_core::repo::RepoError;
use rusqlite::Connection;
use serde::Deserialize;

use crate::repos::lock;
use crate::{exec, query_one, query_vec, Db};

/// `daily_meta` / `daily_category_minutes` / `daily_hour_seconds` /
/// `sleep_nights` / `dirty_local_days` / `report_job_status` persistence.
pub struct SqliteDigestRepo {
    db: Db,
}

impl SqliteDigestRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

fn run_args(
    conn: &Connection,
    sql: &str,
    args: &[String],
) -> Result<(), RepoError> {
    let params: Vec<&dyn rusqlite::types::ToSql> = args
        .iter()
        .map(|arg| arg as &dyn rusqlite::types::ToSql)
        .collect();
    exec(conn, sql, &params)
}

/// The calendar owner of one event, or `None` when the calendar is missing or
/// soft-deleted.
pub fn calendar_owner(conn: &Connection, calendar_id: &str) -> Result<Option<String>, RepoError> {
    #[derive(Deserialize)]
    struct OwnerRow {
        user_id: String,
    }
    let row: Option<OwnerRow> = query_one(
        conn,
        api_core::digest::repo::DIGEST_CALENDAR_OWNER_SQL,
        &[&calendar_id],
    )?;
    Ok(row.map(|row| row.user_id))
}

/// The stored span of one event (no `deleted_at` filter).
pub fn event_span(
    conn: &Connection,
    calendar_id: &str,
    google_event_id: &str,
) -> Result<Option<EventSpanRow>, RepoError> {
    query_one(conn, DIGEST_EVENT_SPAN_SQL, &[&calendar_id, &google_event_id])
}

/// Best-effort dirty marking shared by the SQLite calendar-event writes: the
/// union of the stored and incoming spans, plus their ±1 calendar-day sleep
/// neighbours.
///
/// The error is deliberately swallowed by the callers — the Google cache is
/// the source of truth and a calendar-event write must never fail over a
/// digest bookkeeping row.
pub fn mark_dirty_spans(
    conn: &Connection,
    user_id: &str,
    spans: &[EventSpan<'_>],
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    let dates = api_core::digest::dirty_dates_for_spans(spans);
    if dates.is_empty() {
        return Ok(());
    }
    for chunk in dates.chunks(DIGEST_MARK_DIRTY_CHUNK_SIZE) {
        let (sql, args) = build_mark_dirty_sql(user_id, chunk, now_rfc3339);
        run_args(conn, &sql, &args)?;
    }
    Ok(())
}

#[async_trait::async_trait(?Send)]
impl DigestRepo for SqliteDigestRepo {
    async fn mark_dirty(
        &self,
        user_id: &str,
        dates: &[String],
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        if dates.is_empty() {
            return Ok(());
        }
        let conn = lock(&self.db)?;
        for chunk in dates.chunks(DIGEST_MARK_DIRTY_CHUNK_SIZE) {
            let (sql, args) = build_mark_dirty_sql(user_id, chunk, now_rfc3339);
            run_args(&conn, &sql, &args)?;
        }
        Ok(())
    }

    async fn list_dirty(&self, user_id: &str, limit: usize) -> Result<Vec<String>, RepoError> {
        #[derive(Deserialize)]
        struct DateRow {
            local_date: String,
        }
        let conn = lock(&self.db)?;
        let limit = limit as i64;
        let rows: Vec<DateRow> = query_vec(&conn, DIGEST_LIST_DIRTY_SQL, &[&user_id, &limit])?;
        Ok(rows.into_iter().map(|row| row.local_date).collect())
    }

    async fn count_dirty(&self, user_id: &str) -> Result<i64, RepoError> {
        #[derive(Deserialize)]
        struct CountRow {
            dirty_days: i64,
        }
        let conn = lock(&self.db)?;
        let row: Option<CountRow> =
            query_one(&conn, DIGEST_COUNT_DIRTY_SQL, &[&user_id])?;
        Ok(row.map(|row| row.dirty_days).unwrap_or(0))
    }

    async fn clear_dirty(&self, user_id: &str, dates: &[String]) -> Result<(), RepoError> {
        if dates.is_empty() {
            return Ok(());
        }
        let conn = lock(&self.db)?;
        for date in dates {
            exec(&conn, DIGEST_CLEAR_DIRTY_SQL, &[&user_id, date])?;
        }
        Ok(())
    }

    async fn users_with_work(&self, limit: usize) -> Result<Vec<String>, RepoError> {
        #[derive(Deserialize)]
        struct UserRow {
            user_id: String,
        }
        let conn = lock(&self.db)?;
        let limit = limit as i64;
        let rows: Vec<UserRow> = query_vec(&conn, DIGEST_USERS_WITH_WORK_SQL, &[&limit])?;
        Ok(rows.into_iter().map(|row| row.user_id).collect())
    }

    async fn replace_day(&self, user_id: &str, day: &DayDigest) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        // A logical replace, not a transaction: DELETEs then INSERTs, so a
        // crash midway leaves a partial day that the next dirty rebuild
        // repairs. Idempotence is what matters, and a second identical run
        // leaves identical rows.
        exec(
            &conn,
            DIGEST_DELETE_CATEGORY_SQL,
            &[&user_id, &day.local_date],
        )?;
        exec(&conn, DIGEST_DELETE_HOURS_SQL, &[&user_id, &day.local_date])?;
        exec(&conn, DIGEST_DELETE_NIGHTS_SQL, &[&user_id, &day.local_date])?;

        let category_rows: Vec<(String, i64)> = day
            .category_seconds
            .iter()
            .map(|row| (row.category_id.clone(), row.seconds))
            .collect();
        for chunk in category_rows.chunks(DIGEST_CATEGORY_INSERT_CHUNK_SIZE) {
            let (sql, args) = build_digest_category_insert_sql(user_id, &day.local_date, chunk);
            run_args(&conn, &sql, &args)?;
        }

        let hour_rows: Vec<(i64, String, i64)> = day
            .hour_seconds
            .iter()
            .map(|row| (row.hour, row.category_id.clone(), row.seconds))
            .collect();
        for chunk in hour_rows.chunks(api_core::digest::repo::DIGEST_HOUR_INSERT_CHUNK_SIZE) {
            let (sql, args) = build_digest_hour_insert_sql(user_id, &day.local_date, chunk);
            run_args(&conn, &sql, &args)?;
        }

        let night_rows: Vec<NightInsert> = day
            .nights
            .iter()
            .map(|night| NightInsert {
                wake_date: night.wake_date.clone(),
                asleep_at: night.asleep_at.clone(),
                woke_at: night.woke_at.clone(),
                seconds: night.seconds,
                bed_min_past_noon: night.bed_min_past_noon,
                wake_min_past_midnight: night.wake_min_past_midnight,
                source: night.source.as_str().to_string(),
            })
            .collect();
        for chunk in night_rows.chunks(DIGEST_NIGHT_INSERT_CHUNK_SIZE) {
            let (sql, args) = build_digest_night_insert_sql(user_id, chunk);
            run_args(&conn, &sql, &args)?;
        }

        exec(
            &conn,
            DIGEST_UPSERT_META_SQL,
            &[
                &user_id,
                &day.local_date,
                &day.time_zone,
                &day.tracked_seconds,
                &day.sleep_seconds,
                &api_core::digest::ALGO_VERSION,
                &day.computed_at,
            ],
        )
    }

    async fn load_events_touching(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<api_core::digest::DigestEvent>, RepoError> {
        let conn = lock(&self.db)?;
        let rows: Vec<api_core::digest::DigestEventRow> = query_vec(
            &conn,
            DIGEST_LOAD_EVENTS_SQL,
            &[&user_id, &to_date, &from_date],
        )?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn primary_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct ZoneRow {
            time_zone: String,
        }
        let conn = lock(&self.db)?;
        let row: Option<ZoneRow> =
            query_one(&conn, DIGEST_PRIMARY_TIME_ZONE_SQL, &[&user_id])?;
        Ok(row.map(|row| row.time_zone))
    }

    async fn latest_zone_before(
        &self,
        user_id: &str,
        date: &str,
    ) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct ZoneRow {
            time_zone: String,
        }
        let conn = lock(&self.db)?;
        let row: Option<ZoneRow> =
            query_one(&conn, DIGEST_LATEST_ZONE_BEFORE_SQL, &[&user_id, &date])?;
        Ok(row.map(|row| row.time_zone))
    }

    async fn job_status(&self, user_id: &str, kind: &str) -> Result<Option<JobStatus>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, DIGEST_JOB_STATUS_SQL, &[&user_id, &kind])
    }

    async fn upsert_job_status(
        &self,
        user_id: &str,
        kind: &str,
        status: &str,
        cursor: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            DIGEST_UPSERT_JOB_STATUS_SQL,
            &[&user_id, &kind, &status, &cursor, &now_rfc3339],
        )
    }

    async fn reclassify_batch(
        &self,
        user_id: &str,
        after_id: &str,
        limit: usize,
    ) -> Result<Vec<ReclassifyRow>, RepoError> {
        let conn = lock(&self.db)?;
        let limit = limit as i64;
        query_vec(
            &conn,
            DIGEST_RECLASSIFY_BATCH_SQL,
            &[&user_id, &after_id, &limit],
        )
    }

    async fn set_category(&self, id: &str, category_id: Option<&str>) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        let category = category_id.unwrap_or("");
        exec(&conn, DIGEST_SET_CATEGORY_SQL, &[&category, &id])
    }

    async fn has_meta(&self, user_id: &str) -> Result<bool, RepoError> {
        #[derive(Deserialize)]
        struct PresentRow {
            #[allow(dead_code)]
            present: i64,
        }
        let conn = lock(&self.db)?;
        let row: Option<PresentRow> = query_one(&conn, DIGEST_HAS_META_SQL, &[&user_id])?;
        Ok(row.is_some())
    }

    async fn has_stale_meta(&self, user_id: &str, algo_version: i64) -> Result<bool, RepoError> {
        #[derive(Deserialize)]
        struct PresentRow {
            #[allow(dead_code)]
            present: i64,
        }
        let conn = lock(&self.db)?;
        let row: Option<PresentRow> =
            query_one(&conn, DIGEST_HAS_STALE_META_SQL, &[&user_id, &algo_version])?;
        Ok(row.is_some())
    }

    async fn earliest_event_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct EarliestRow {
            earliest_date: Option<String>,
        }
        let conn = lock(&self.db)?;
        let row: Option<EarliestRow> =
            query_one(&conn, DIGEST_EARLIEST_EVENT_DATE_SQL, &[&user_id])?;
        Ok(row.and_then(|row| row.earliest_date))
    }
}

/// Counts rows in one digest table (test helper).
pub fn count_rows(conn: &Connection, table: &str) -> Result<i64, RepoError> {
    let sql = format!("SELECT COUNT(*) AS n FROM {table}");
    #[derive(Deserialize)]
    struct CountRow {
        n: i64,
    }
    let row: Option<CountRow> = query_one(conn, &sql, &[])?;
    Ok(row.map(|row| row.n).unwrap_or(0))
}
