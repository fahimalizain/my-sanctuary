//! D1-backed digest repository (reports slice 2).
//!
//! Bind order mirrors `packages/d1-sync/src/digest.rs` and the SQL constants
//! live in `api_core::digest::repo`, so a statement that works locally against
//! the real migrations is the one that runs here.
//!
//! Two rules from the brief show up in this file:
//!
//! 1. **Best-effort dirty marking.** [`mark_dirty_for_calendar_event`] never
//!    fails the calendar write it hangs off — a lost mark only delays a
//!    rebuild, while a failed event write loses Google data. Failures are
//!    logged loudly instead.
//! 2. **Bounded batches.** Every statement stays under D1's 100-bound-parameter
//!    limit through the chunked builders.

use api_core::digest::repo::{
    build_digest_category_insert_sql, build_digest_hour_insert_sql, build_digest_night_insert_sql,
    build_event_spans_by_google_ids_sql, build_mark_dirty_sql, DigestEventRow, DigestRepo,
    EventSpan, EventSpanKeyRow, EventSpanRow, JobStatus, NightInsert, ReclassifyRow,
    DIGEST_CALENDAR_EVENT_SPANS_SQL, DIGEST_CALENDAR_OWNER_SQL, DIGEST_CATEGORY_INSERT_CHUNK_SIZE,
    DIGEST_CLEAR_DIRTY_SQL, DIGEST_COUNT_DIRTY_SQL, DIGEST_DELETE_CATEGORY_SQL,
    DIGEST_DELETE_HOURS_SQL, DIGEST_DELETE_NIGHTS_SQL, DIGEST_EARLIEST_EVENT_DATE_SQL,
    DIGEST_EVENT_SPAN_SQL, DIGEST_HAS_META_SQL, DIGEST_HAS_STALE_META_SQL,
    DIGEST_HAS_UNSTAMPED_SQL, DIGEST_HOUR_INSERT_CHUNK_SIZE, DIGEST_JOB_STATUS_SQL,
    DIGEST_LATEST_TIME_ZONE_SQL, DIGEST_LATEST_ZONE_BEFORE_SQL, DIGEST_LIST_DIRTY_SQL,
    DIGEST_LOAD_EVENTS_SQL, DIGEST_MARK_DIRTY_CHUNK_SIZE, DIGEST_NIGHT_INSERT_CHUNK_SIZE,
    DIGEST_PRIMARY_TIME_ZONE_SQL, DIGEST_RECLASSIFY_BATCH_SQL, DIGEST_SET_CATEGORY_SQL,
    DIGEST_SLEEP_TARGET_PARAMS_SQL, DIGEST_UNSTAMPED_BATCH_SQL, DIGEST_UPSERT_JOB_STATUS_SQL,
    DIGEST_UPSERT_META_SQL, DIGEST_USERS_WITH_WORK_SQL,
};
use api_core::digest::{
    dirty_dates_for_spans, sleep_target_secs_from_params, DayDigest, DigestEvent,
};
use api_core::repo::RepoError;
use serde::Deserialize;
use worker::{D1Database, D1Type};

use super::{backend, bind_text, query_vec, run_stmt};

/// `daily_meta` / `daily_category_minutes` / `daily_hour_seconds` /
/// `sleep_nights` / `dirty_local_days` / `report_job_status` persistence.
pub struct D1DigestRepo {
    db: D1Database,
}

impl D1DigestRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// One error class for the "row exists" probes.
#[derive(Debug, Deserialize)]
struct PresentRow {
    #[allow(dead_code)]
    present: i64,
}

#[derive(Debug, Deserialize)]
struct OwnerRow {
    user_id: String,
}

/// The owner of one calendar, or `None` when it is missing/soft-deleted (the
/// dirty mark is skipped: a calendar write must never fail over bookkeeping).
pub(crate) async fn calendar_owner(
    db: &D1Database,
    calendar_id: &str,
) -> Result<Option<String>, RepoError> {
    let stmt = db
        .prepare(DIGEST_CALENDAR_OWNER_SQL)
        .bind_refs(&[D1Type::Text(calendar_id)])
        .map_err(backend)?;
    let row: Option<OwnerRow> = stmt.first::<OwnerRow>(None).await.map_err(backend)?;
    Ok(row.map(|row| row.user_id))
}

/// Marks `dates` dirty for one user, chunked under D1's 100-bind limit — one
/// statement per chunk, so a whole batch's union costs the fewest round trips
/// the limit allows.
pub(crate) async fn mark_dirty_dates(
    db: &D1Database,
    user_id: &str,
    dates: &[String],
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    for chunk in dates.chunks(DIGEST_MARK_DIRTY_CHUNK_SIZE) {
        let (sql, args) = build_mark_dirty_sql(user_id, chunk, now_rfc3339);
        let stmt = db
            .prepare(&sql)
            .bind_refs(&bind_text(&args))
            .map_err(backend)?;
        run_stmt(stmt).await?;
    }
    Ok(())
}

/// Best-effort dirty marking for one calendar event write.
///
/// Unions the stored span (when the row already exists) with the incoming
/// span, adds the ±1 calendar-day sleep neighbours, and upserts the result in
/// chunks. Returns `Err` for the caller to *log* — callers must not fail the
/// event write because of it (the Google cache is the source of truth; cron
/// cannot see days nobody dirtied, so the failure is loud instead).
pub(crate) async fn mark_dirty_for_calendar_event(
    db: &D1Database,
    calendar_id: &str,
    old_span: Option<&EventSpanRow>,
    new_span: Option<EventSpan<'_>>,
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    let Some(owner) = calendar_owner(db, calendar_id).await? else {
        // A missing/soft-deleted calendar skips the mark.
        return Ok(());
    };
    let mut spans: Vec<EventSpan<'_>> = Vec::new();
    if let Some(span) = old_span {
        spans.push(span.as_span());
    }
    if let Some(span) = new_span {
        spans.push(span);
    }
    mark_dirty_span_union(db, &owner, &spans, now_rfc3339).await
}

/// Marks the union of `spans` dirty for one user (chunked), the shared tail of
/// every event-write path.
pub(crate) async fn mark_dirty_span_union(
    db: &D1Database,
    user_id: &str,
    spans: &[EventSpan<'_>],
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    let dates = dirty_dates_for_spans(spans);
    if dates.is_empty() {
        return Ok(());
    }
    mark_dirty_dates(db, user_id, &dates, now_rfc3339).await
}

/// Dirty-marks every local date a **soft-deleted** calendar's living events
/// touch.
///
/// The digest drops a soft-deleted calendar's events (`DIGEST_LOAD_EVENTS_SQL`
/// filters `c.deleted_at IS NULL`), so the days those events used to fill must
/// be rebuilt once the calendar goes away. One SELECT reads every living span
/// and the exact per-event union is marked (each event's own zone, ±1 day): a
/// calendar delete is rare, so the precision is worth more than the cheaper
/// min/max closed range that would also dirty the days in between.
///
/// The owner must be resolved by the caller **before** the soft delete — the
/// calendar is invisible to [`calendar_owner`] afterwards.
pub(crate) async fn mark_dirty_for_calendar_delete(
    db: &D1Database,
    user_id: &str,
    calendar_id: &str,
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    let stmt = db
        .prepare(DIGEST_CALENDAR_EVENT_SPANS_SQL)
        .bind_refs(&[D1Type::Text(calendar_id)])
        .map_err(backend)?;
    let rows: Vec<EventSpanRow> = query_vec(stmt).await?;
    let spans: Vec<EventSpan<'_>> = rows.iter().map(EventSpanRow::as_span).collect();
    mark_dirty_span_union(db, user_id, &spans, now_rfc3339).await
}

/// The stored spans of several events of one calendar, in one SELECT
/// (`google_event_id IN (…)`) — the batch upsert's replacement for one
/// [`stored_event_span`] per event.
pub(crate) async fn stored_event_spans(
    db: &D1Database,
    calendar_id: &str,
    google_event_ids: &[String],
) -> Result<Vec<EventSpanKeyRow>, RepoError> {
    if google_event_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (sql, args) = build_event_spans_by_google_ids_sql(calendar_id, google_event_ids);
    let stmt = db
        .prepare(&sql)
        .bind_refs(&bind_text(&args))
        .map_err(backend)?;
    query_vec(stmt).await
}

/// The stored span of one event, or `None` when the row does not exist.
pub(crate) async fn stored_event_span(
    db: &D1Database,
    calendar_id: &str,
    google_event_id: &str,
) -> Result<Option<EventSpanRow>, RepoError> {
    let stmt = db
        .prepare(DIGEST_EVENT_SPAN_SQL)
        .bind_refs(&[
            D1Type::Text(calendar_id),
            D1Type::Text(google_event_id),
        ])
        .map_err(backend)?;
    stmt.first::<EventSpanRow>(None).await.map_err(backend)
}

#[async_trait::async_trait(?Send)]
impl DigestRepo for D1DigestRepo {
    async fn mark_dirty(
        &self,
        user_id: &str,
        dates: &[String],
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        if dates.is_empty() {
            return Ok(());
        }
        mark_dirty_dates(&self.db, user_id, dates, now_rfc3339).await
    }

    async fn list_dirty(&self, user_id: &str, limit: usize) -> Result<Vec<String>, RepoError> {
        #[derive(Deserialize)]
        struct DateRow {
            local_date: String,
        }
        let stmt = self
            .db
            .prepare(DIGEST_LIST_DIRTY_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Integer(limit.min(i32::MAX as usize) as i32),
            ])
            .map_err(backend)?;
        let rows: Vec<DateRow> = query_vec(stmt).await?;
        Ok(rows.into_iter().map(|row| row.local_date).collect())
    }

    async fn count_dirty(&self, user_id: &str) -> Result<i64, RepoError> {
        #[derive(Deserialize)]
        struct CountRow {
            dirty_days: i64,
        }
        let stmt = self
            .db
            .prepare(DIGEST_COUNT_DIRTY_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<CountRow> = stmt.first::<CountRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.dirty_days).unwrap_or(0))
    }

    async fn clear_dirty(&self, user_id: &str, dates: &[String]) -> Result<(), RepoError> {
        for date in dates {
            let stmt = self
                .db
                .prepare(DIGEST_CLEAR_DIRTY_SQL)
                .bind_refs(&[D1Type::Text(user_id), D1Type::Text(date)])
                .map_err(backend)?;
            run_stmt(stmt).await?;
        }
        Ok(())
    }

    async fn users_with_work(&self, limit: usize) -> Result<Vec<String>, RepoError> {
        #[derive(Deserialize)]
        struct UserRow {
            user_id: String,
        }
        let stmt = self
            .db
            .prepare(DIGEST_USERS_WITH_WORK_SQL)
            .bind_refs(&[D1Type::Integer(limit.min(i32::MAX as usize) as i32)])
            .map_err(backend)?;
        let rows: Vec<UserRow> = query_vec(stmt).await?;
        Ok(rows.into_iter().map(|row| row.user_id).collect())
    }

    async fn replace_day(&self, user_id: &str, day: &DayDigest) -> Result<(), RepoError> {
        // A logical replace, not a transaction: DELETEs then INSERTs, so a
        // crash midway leaves a partial day that the next dirty rebuild
        // repairs. Idempotence is what matters — a second identical run leaves
        // identical rows.
        for sql in [
            DIGEST_DELETE_CATEGORY_SQL,
            DIGEST_DELETE_HOURS_SQL,
            DIGEST_DELETE_NIGHTS_SQL,
        ] {
            let stmt = self
                .db
                .prepare(sql)
                .bind_refs(&[D1Type::Text(user_id), D1Type::Text(&day.local_date)])
                .map_err(backend)?;
            run_stmt(stmt).await?;
        }

        let category_rows: Vec<(String, i64)> = day
            .category_seconds
            .iter()
            .map(|row| (row.category_id.clone(), row.seconds))
            .collect();
        for chunk in category_rows.chunks(DIGEST_CATEGORY_INSERT_CHUNK_SIZE) {
            let (sql, args) = build_digest_category_insert_sql(user_id, &day.local_date, chunk);
            let stmt = self
                .db
                .prepare(&sql)
                .bind_refs(&bind_text(&args))
                .map_err(backend)?;
            run_stmt(stmt).await?;
        }

        let hour_rows: Vec<(i64, String, i64)> = day
            .hour_seconds
            .iter()
            .map(|row| (row.hour, row.category_id.clone(), row.seconds))
            .collect();
        for chunk in hour_rows.chunks(DIGEST_HOUR_INSERT_CHUNK_SIZE) {
            let (sql, args) = build_digest_hour_insert_sql(user_id, &day.local_date, chunk);
            let stmt = self
                .db
                .prepare(&sql)
                .bind_refs(&bind_text(&args))
                .map_err(backend)?;
            run_stmt(stmt).await?;
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
            let stmt = self
                .db
                .prepare(&sql)
                .bind_refs(&bind_text(&args))
                .map_err(backend)?;
            run_stmt(stmt).await?;
        }

        let algo_version = api_core::ALGO_VERSION.to_string();
        let tracked = day.tracked_seconds.to_string();
        let sleep = day.sleep_seconds.to_string();
        let stmt = self
            .db
            .prepare(DIGEST_UPSERT_META_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(&day.local_date),
                D1Type::Text(&day.time_zone),
                D1Type::Text(&tracked),
                D1Type::Text(&sleep),
                D1Type::Text(&algo_version),
                D1Type::Text(&day.computed_at),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn load_events_touching(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<DigestEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_LOAD_EVENTS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(to_date),
                D1Type::Text(from_date),
            ])
            .map_err(backend)?;
        let rows: Vec<DigestEventRow> = query_vec(stmt).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn primary_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct ZoneRow {
            time_zone: String,
        }
        let stmt = self
            .db
            .prepare(DIGEST_PRIMARY_TIME_ZONE_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<ZoneRow> = stmt.first::<ZoneRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.time_zone))
    }

    async fn latest_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct ZoneRow {
            time_zone: String,
        }
        let stmt = self
            .db
            .prepare(DIGEST_LATEST_TIME_ZONE_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<ZoneRow> = stmt.first::<ZoneRow>(None).await.map_err(backend)?;
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
        let stmt = self
            .db
            .prepare(DIGEST_LATEST_ZONE_BEFORE_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(date)])
            .map_err(backend)?;
        let row: Option<ZoneRow> = stmt.first::<ZoneRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.time_zone))
    }

    async fn job_status(&self, user_id: &str, kind: &str) -> Result<Option<JobStatus>, RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_JOB_STATUS_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(kind)])
            .map_err(backend)?;
        stmt.first::<JobStatus>(None).await.map_err(backend)
    }

    async fn upsert_job_status(
        &self,
        user_id: &str,
        kind: &str,
        status: &str,
        cursor: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_UPSERT_JOB_STATUS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(kind),
                D1Type::Text(status),
                D1Type::Text(cursor),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn reclassify_batch(
        &self,
        user_id: &str,
        after_id: &str,
        limit: usize,
    ) -> Result<Vec<ReclassifyRow>, RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_RECLASSIFY_BATCH_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(after_id),
                D1Type::Integer(limit.min(i32::MAX as usize) as i32),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn set_category(&self, id: &str, category_id: Option<&str>) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_SET_CATEGORY_SQL)
            .bind_refs(&[
                D1Type::Text(category_id.unwrap_or("")),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn unstamped_batch(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<ReclassifyRow>, RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_UNSTAMPED_BATCH_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Integer(limit.min(i32::MAX as usize) as i32),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn has_unstamped_event(&self, user_id: &str) -> Result<bool, RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_HAS_UNSTAMPED_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<PresentRow> = stmt.first::<PresentRow>(None).await.map_err(backend)?;
        Ok(row.is_some())
    }

    async fn sleep_target_secs(&self, user_id: &str) -> Result<Option<i64>, RepoError> {
        #[derive(Deserialize)]
        struct ParamsRow {
            params_json: String,
        }
        let stmt = self
            .db
            .prepare(DIGEST_SLEEP_TARGET_PARAMS_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<ParamsRow> = stmt.first::<ParamsRow>(None).await.map_err(backend)?;
        Ok(row.and_then(|row| sleep_target_secs_from_params(&row.params_json)))
    }

    async fn has_meta(&self, user_id: &str) -> Result<bool, RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_HAS_META_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<PresentRow> = stmt.first::<PresentRow>(None).await.map_err(backend)?;
        Ok(row.is_some())
    }

    async fn has_stale_meta(&self, user_id: &str, algo_version: i64) -> Result<bool, RepoError> {
        let stmt = self
            .db
            .prepare(DIGEST_HAS_STALE_META_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(&algo_version.to_string()),
            ])
            .map_err(backend)?;
        let row: Option<PresentRow> = stmt.first::<PresentRow>(None).await.map_err(backend)?;
        Ok(row.is_some())
    }

    async fn earliest_event_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct EarliestRow {
            earliest_date: Option<String>,
        }
        let stmt = self
            .db
            .prepare(DIGEST_EARLIEST_EVENT_DATE_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<EarliestRow> = stmt.first::<EarliestRow>(None).await.map_err(backend)?;
        Ok(row.and_then(|row| row.earliest_date))
    }
}
