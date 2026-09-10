//! D1-backed calendar repository implementations.

use api_core::models::{
    CalendarEvent, CalendarEventOperation, GoogleCalendar, NewCalendar, NewCalendarEvent,
    NewCalendarEventOperation, NewWatchChannel, WatchChannel,
};
use api_core::repo::{
    build_event_upsert_if_owner_sql, build_event_upsert_sql, build_operation_list_by_statuses_sql,
    build_replica_seen_insert_sql, CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo,
    RepoError, WatchChannelRepo, CALENDAR_BEGIN_REPLICA_RESEED_SQL,
    CALENDAR_BUMP_DIRTY_REQUESTED_SQL, CALENDAR_DELETE_SQL, CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL,
    CALENDAR_GET_BY_ID_SQL, CALENDAR_GET_BY_ID_UNFILTERED_SQL, CALENDAR_LEASE_HELD_SQL,
    CALENDAR_LIST_BY_USER_ID_SQL, CALENDAR_LIST_STATE_GET_SQL, CALENDAR_LIST_STATE_UPSERT_SQL,
    CALENDAR_LIST_SYNC_ENABLED_SQL, CALENDAR_LIST_USER_IDS_SQL, CALENDAR_MARK_DIRTY_APPLIED_SQL,
    CALENDAR_RECORD_SYNC_ATTEMPT_SQL, CALENDAR_RECORD_SYNC_FAILURE_SQL,
    CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL, CALENDAR_RECORD_SYNC_SUCCESS_SQL,
    CALENDAR_RELEASE_LEASE_SQL, CALENDAR_RENEW_LEASE_SQL, CALENDAR_SET_EVENT_LABELS_SQL,
    CALENDAR_SET_SYNC_ENABLED_SQL, CALENDAR_SET_WATCH_COVERAGE_SQL, CALENDAR_TRY_ACQUIRE_LEASE_SQL,
    CALENDAR_UPDATE_SYNC_STATE_SQL, CALENDAR_UPSERT_SQL,
    EVENT_CLEAR_REPLICA_SEEN_FOR_CALENDAR_SQL, EVENT_DELETE_BY_GOOGLE_EVENT_ID_IF_OWNER_SQL,
    EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL, EVENT_DELETE_SQL, EVENT_DELETE_STALE_SQL,
    EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL, EVENT_GET_BY_ID_SQL, EVENT_GET_ID_BY_NATURAL_KEY_SQL,
    EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL, EVENT_LIST_RUNNING_BY_USER_ID_SQL,
    EVENT_SWEEP_ABSENT_IF_OWNER_SQL, EVENT_UPSERT_CHUNK_SIZE, OPERATION_GET_BY_ID_SQL,
    OPERATION_INSERT_SQL, OPERATION_LIST_INFLIGHT_GOOGLE_IDS_SQL, OPERATION_UPDATE_PROGRESS_SQL,
    OPERATION_UPDATE_STATUS_SQL, REPLICA_SEEN_INSERT_CHUNK_SIZE,
    WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL, WATCH_CHANNEL_DELETE_BY_ID_SQL,
    WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL, WATCH_CHANNEL_INSERT_SQL, WATCH_CHANNEL_LIST_ALL_SQL,
    WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL, WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL,
};
use serde::Deserialize;
use worker::{D1Database, D1Type};

use super::{backend, now_rfc3339, query_vec, run_stmt, run_stmt_changes};

/// `google_calendars` table persistence.
pub struct D1CalendarRepo {
    db: D1Database,
}

impl D1CalendarRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// `calendar_events` table persistence.
pub struct D1CalendarEventRepo {
    db: D1Database,
}

impl D1CalendarEventRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// `google_calendars_watch_channels` table persistence.
pub struct D1WatchChannelRepo {
    db: D1Database,
}

impl D1WatchChannelRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// `calendar_event_operations` outbound write journal (issue #50 / Vertical 4).
pub struct D1CalendarEventOperationRepo {
    db: D1Database,
}

impl D1CalendarEventOperationRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}


#[async_trait::async_trait(?Send)]
impl CalendarRepo for D1CalendarRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_LIST_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_LIST_SYNC_ENABLED_SQL)
            .bind(&[])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<GoogleCalendar>(None).await.map_err(backend)
    }

    async fn get_by_id_unfiltered(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_GET_BY_ID_UNFILTERED_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<GoogleCalendar>(None).await.map_err(backend)
    }

    async fn get_by_google_cal_id(
        &self,
        user_id: &str,
        google_cal_id: &str,
    ) -> Result<Option<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(google_cal_id)])
            .map_err(backend)?;
        stmt.first::<GoogleCalendar>(None).await.map_err(backend)
    }

    async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError> {
        self.upsert_batch(vec![calendar]).await
    }

    async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
        for calendar in calendars {
            let id = uuid::Uuid::new_v4().to_string();
            let now = now_rfc3339();
            // `is_primary`/`sync_enabled` are INTEGER 0/1 columns in D1.
            let last_synced_at = match calendar.last_synced_at.as_deref() {
                Some(value) => D1Type::Text(value),
                // NULL flows through COALESCE(NULLIF(..., ''), …) and keeps any
                // previously stored value.
                None => D1Type::Null,
            };
            let stmt = self
                .db
                .prepare(CALENDAR_UPSERT_SQL)
                .bind_refs(&[
                    D1Type::Text(&id),
                    D1Type::Text(&calendar.user_id),
                    D1Type::Text(&calendar.google_calendar_id),
                    D1Type::Text(&calendar.summary),
                    D1Type::Text(&calendar.time_zone),
                    D1Type::Integer(i32::from(calendar.is_primary)),
                    D1Type::Text(&calendar.access_role),
                    D1Type::Integer(i32::from(calendar.sync_enabled)),
                    D1Type::Text(&calendar.sync_token),
                    last_synced_at,
                    D1Type::Text(&now),
                    D1Type::Text(&now),
                ])
                .map_err(backend)?;
            run_stmt(stmt).await?;
        }
        Ok(())
    }

    async fn update_sync_state(
        &self,
        id: &str,
        sync_token: &str,
        last_synced_at_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let now = now_rfc3339();
        let stmt = self
            .db
            .prepare(CALENDAR_UPDATE_SYNC_STATE_SQL)
            .bind_refs(&[
                D1Type::Text(sync_token),
                D1Type::Text(last_synced_at_rfc3339),
                D1Type::Text(&now),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn record_sync_attempt(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_RECORD_SYNC_ATTEMPT_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn record_sync_success(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // Binds: token, last_synced_at, last_success_at, last_attempt_at,
        // fingerprint, updated_at, id — both success timestamps share `now`.
        let stmt = self
            .db
            .prepare(CALENDAR_RECORD_SYNC_SUCCESS_SQL)
            .bind_refs(&[
                D1Type::Text(sync_token),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(query_fingerprint),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn record_sync_success_if_owner(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        // Binds: token, last_synced_at, last_success_at, last_attempt_at,
        // fingerprint, updated_at, id, lease_owner, now (lease check).
        let stmt = self
            .db
            .prepare(CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL)
            .bind_refs(&[
                D1Type::Text(sync_token),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(query_fingerprint),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
                D1Type::Text(lease_owner),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        let changes = run_stmt_changes(stmt).await?;
        Ok(changes > 0)
    }

    async fn record_sync_failure(
        &self,
        id: &str,
        error_code: &str,
        sync_status: &str,
        next_retry_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_RECORD_SYNC_FAILURE_SQL)
            .bind_refs(&[
                D1Type::Text(error_code),
                D1Type::Text(sync_status),
                D1Type::Text(next_retry_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn begin_replica_reseed(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        // Binds: updated_at, id.
        let stmt = self
            .db
            .prepare(CALENDAR_BEGIN_REPLICA_RESEED_SQL)
            .bind_refs(&[D1Type::Text(now_rfc3339), D1Type::Text(id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn try_acquire_lease(
        &self,
        id: &str,
        owner: &str,
        now_rfc3339: &str,
        expires_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        // Binds: owner, expires, now, id, owner, now.
        let stmt = self
            .db
            .prepare(CALENDAR_TRY_ACQUIRE_LEASE_SQL)
            .bind_refs(&[
                D1Type::Text(owner),
                D1Type::Text(expires_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
                D1Type::Text(owner),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        // Verify ownership (another writer may have raced; changes alone is not
        // enough if the WHERE matched a different concurrent steal).
        match self.get_by_id(id).await? {
            Some(cal) => Ok(cal.lease_owner == owner),
            None => Ok(false),
        }
    }

    async fn release_lease(
        &self,
        id: &str,
        owner: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_RELEASE_LEASE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
                D1Type::Text(owner),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn renew_lease(
        &self,
        id: &str,
        owner: &str,
        expires_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_RENEW_LEASE_SQL)
            .bind_refs(&[
                D1Type::Text(expires_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
                D1Type::Text(owner),
            ])
            .map_err(backend)?;
        let changes = run_stmt_changes(stmt).await?;
        Ok(changes > 0)
    }

    async fn bump_dirty_requested(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_BUMP_DIRTY_REQUESTED_SQL)
            .bind_refs(&[D1Type::Text(now_rfc3339), D1Type::Text(id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn mark_dirty_applied(
        &self,
        id: &str,
        generation: i64,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // D1 Integer is i32; dirty generations are monotonic counters well
        // within that range for practical workloads.
        let gen = generation as i32;
        let stmt = self
            .db
            .prepare(CALENDAR_MARK_DIRTY_APPLIED_SQL)
            .bind_refs(&[
                D1Type::Integer(gen),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
                D1Type::Integer(gen),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn set_sync_enabled(
        &self,
        id: &str,
        enabled: bool,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_SET_SYNC_ENABLED_SQL)
            .bind_refs(&[
                D1Type::Integer(i32::from(enabled)),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn set_event_labels(
        &self,
        id: &str,
        event_labels_json: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_SET_EVENT_LABELS_SQL)
            .bind_refs(&[
                D1Type::Text(event_labels_json),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn set_watch_coverage(
        &self,
        id: &str,
        coverage: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // Binds: coverage, now, id.
        let stmt = self
            .db
            .prepare(CALENDAR_SET_WATCH_COVERAGE_SQL)
            .bind_refs(&[
                D1Type::Text(coverage),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn get_calendar_list_sync_token(
        &self,
        user_id: &str,
    ) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct Row {
            sync_token: String,
        }
        let stmt = self
            .db
            .prepare(CALENDAR_LIST_STATE_GET_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row = stmt.first::<Row>(None).await.map_err(backend)?;
        Ok(row.map(|r| r.sync_token))
    }

    async fn set_calendar_list_sync_token(
        &self,
        user_id: &str,
        token: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_LIST_STATE_UPSERT_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(token),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn list_user_ids_with_calendars(&self) -> Result<Vec<String>, RepoError> {
        #[derive(Deserialize)]
        struct Row {
            user_id: String,
        }
        let stmt = self
            .db
            .prepare(CALENDAR_LIST_USER_IDS_SQL)
            .bind(&[])
            .map_err(backend)?;
        let rows: Vec<Row> = query_vec(stmt).await?;
        Ok(rows.into_iter().map(|r| r.user_id).collect())
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarEventRepo for D1CalendarEventRepo {
    async fn upsert(&self, event: NewCalendarEvent, now_rfc3339: &str) -> Result<String, RepoError> {
        // Reuse the persisted natural-key id (including soft-deleted) so ON
        // CONFLICT updates the living/deleted row and we return that id —
        // never a discarded candidate UUID after conflict.
        let id = match self
            .lookup_id_by_natural_key(&event.calendar_id, &event.google_event_id)
            .await?
        {
            Some(existing) => existing,
            None => uuid::Uuid::new_v4().to_string(),
        };
        let (sql, args) = build_event_upsert_sql(&[event], now_rfc3339, vec![id.clone()]);
        self.run_upsert(&sql, &args).await?;
        Ok(id)
    }

    async fn upsert_batch(
        &self,
        events: Vec<NewCalendarEvent>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // Chunk to stay under D1's 100 bound-parameter limit (4 rows of 23
        // columns per statement); each chunk is one D1 subrequest. Look up
        // each natural key first so ON CONFLICT hits the living/deleted row
        // instead of inserting a colliding id that gets thrown away.
        for chunk in events.chunks(EVENT_UPSERT_CHUNK_SIZE) {
            let mut ids = Vec::with_capacity(chunk.len());
            for event in chunk {
                let id = match self
                    .lookup_id_by_natural_key(&event.calendar_id, &event.google_event_id)
                    .await?
                {
                    Some(existing) => existing,
                    None => uuid::Uuid::new_v4().to_string(),
                };
                ids.push(id);
            }
            let (sql, args) = build_event_upsert_sql(chunk, now_rfc3339, ids);
            self.run_upsert(&sql, &args).await?;
        }
        Ok(())
    }

    async fn upsert_batch_if_owner(
        &self,
        events: Vec<NewCalendarEvent>,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        if events.is_empty() {
            return Ok(true);
        }
        // Same natural-key lookup + chunking as upsert_batch; fence binds add
        // only 3 params per statement so chunk size stays 4 (92 + 3 = 95).
        for chunk in events.chunks(EVENT_UPSERT_CHUNK_SIZE) {
            let calendar_id = chunk[0].calendar_id.as_str();
            let mut ids = Vec::with_capacity(chunk.len());
            for event in chunk {
                let id = match self
                    .lookup_id_by_natural_key(&event.calendar_id, &event.google_event_id)
                    .await?
                {
                    Some(existing) => existing,
                    None => uuid::Uuid::new_v4().to_string(),
                };
                ids.push(id);
            }
            let (sql, args) = build_event_upsert_if_owner_sql(
                chunk,
                now_rfc3339,
                ids,
                calendar_id,
                lease_owner,
            );
            let changes = self.run_upsert_changes(&sql, &args).await?;
            if changes == 0 {
                // Lease missing/stolen/expired: INSERT SELECT wrote nothing.
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<CalendarEvent>(None).await.map_err(backend)
    }

    async fn get_by_calendar_and_google_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id), D1Type::Text(google_event_id)])
            .map_err(backend)?;
        stmt.first::<CalendarEvent>(None).await.map_err(backend)
    }

    async fn list_by_user_id_and_time_range(
        &self,
        user_id: &str,
        start_rfc3339: &str,
        end_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(end_rfc3339),
                D1Type::Text(start_rfc3339),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_running_by_user_id(
        &self,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_LIST_RUNNING_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(now_rfc3339), D1Type::Text(now_rfc3339)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete_by_google_event_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(calendar_id),
                D1Type::Text(google_event_id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete_by_google_event_id_if_owner(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        // Fenced UPDATE first so a lost owner cannot mutate even if the
        // subsequent lease probe races. changes==0 is ambiguous (no living
        // event vs lease lost) — probe lease ownership for the return value.
        let stmt = self
            .db
            .prepare(EVENT_DELETE_BY_GOOGLE_EVENT_ID_IF_OWNER_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(calendar_id),
                D1Type::Text(google_event_id),
                D1Type::Text(lease_owner),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;

        let probe = self
            .db
            .prepare(CALENDAR_LEASE_HELD_SQL)
            .bind_refs(&[
                D1Type::Text(calendar_id),
                D1Type::Text(lease_owner),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        let held: Option<LeaseHeldRow> = probe.first(None).await.map_err(backend)?;
        Ok(held.is_some())
    }

    async fn delete_stale(
        &self,
        calendar_id: &str,
        older_than_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_DELETE_STALE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(calendar_id),
                D1Type::Text(older_than_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn record_replica_seen(
        &self,
        calendar_id: &str,
        run_id: &str,
        google_event_ids: Vec<String>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        if google_event_ids.is_empty() {
            return Ok(());
        }
        for chunk in google_event_ids.chunks(REPLICA_SEEN_INSERT_CHUNK_SIZE) {
            let (sql, args) =
                build_replica_seen_insert_sql(calendar_id, run_id, chunk, now_rfc3339);
            self.run_upsert(&sql, &args).await?;
        }
        Ok(())
    }

    async fn clear_replica_seen_for_calendar(&self, calendar_id: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_CLEAR_REPLICA_SEEN_FOR_CALENDAR_SQL)
            .bind_refs(&[D1Type::Text(calendar_id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn sweep_absent_if_owner(
        &self,
        calendar_id: &str,
        run_id: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        // Fenced UPDATE first; changes==0 is ambiguous (no ghosts vs lease
        // lost) — probe lease ownership for the return value.
        let stmt = self
            .db
            .prepare(EVENT_SWEEP_ABSENT_IF_OWNER_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(calendar_id),
                D1Type::Text(calendar_id),
                D1Type::Text(run_id),
                D1Type::Text(lease_owner),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;

        let probe = self
            .db
            .prepare(CALENDAR_LEASE_HELD_SQL)
            .bind_refs(&[
                D1Type::Text(calendar_id),
                D1Type::Text(lease_owner),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        let held: Option<LeaseHeldRow> = probe.first(None).await.map_err(backend)?;
        Ok(held.is_some())
    }
}

/// Row projection for `SELECT id …` natural-key lookup.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct EventIdRow {
    id: String,
}

/// Row projection for `CALENDAR_LEASE_HELD_SQL` (`SELECT 1 AS ok …`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct LeaseHeldRow {
    ok: i64,
}

impl D1CalendarEventRepo {
    /// Binds all-string args as `D1Type::Text` and runs the statement.
    async fn run_upsert(&self, sql: &str, args: &[String]) -> Result<(), RepoError> {
        let refs: Vec<D1Type> = args.iter().map(|arg| D1Type::Text(arg)).collect();
        let stmt = self.db.prepare(sql).bind_refs(&refs).map_err(backend)?;
        run_stmt(stmt).await
    }

    /// Like [`Self::run_upsert`] but returns D1 `changes` (rows written).
    async fn run_upsert_changes(&self, sql: &str, args: &[String]) -> Result<usize, RepoError> {
        let refs: Vec<D1Type> = args.iter().map(|arg| D1Type::Text(arg)).collect();
        let stmt = self.db.prepare(sql).bind_refs(&refs).map_err(backend)?;
        run_stmt_changes(stmt).await
    }

    /// Id for `(calendar_id, google_event_id)`, including soft-deleted rows.
    async fn lookup_id_by_natural_key(
        &self,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<String>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_GET_ID_BY_NATURAL_KEY_SQL)
            .bind_refs(&[D1Type::Text(calendar_id), D1Type::Text(google_event_id)])
            .map_err(backend)?;
        Ok(stmt
            .first::<EventIdRow>(None)
            .await
            .map_err(backend)?
            .map(|row| row.id))
    }
}

#[async_trait::async_trait(?Send)]
impl WatchChannelRepo for D1WatchChannelRepo {
    async fn insert(
        &self,
        channel: NewWatchChannel,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&channel.calendar_id),
                D1Type::Text(&channel.channel_id),
                D1Type::Text(&channel.resource_id),
                D1Type::Text(&channel.token),
                D1Type::Text(&channel.expiration),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        Ok(id)
    }

    async fn get_by_channel_id(&self, channel_id: &str) -> Result<Option<WatchChannel>, RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL)
            .bind_refs(&[D1Type::Text(channel_id)])
            .map_err(backend)?;
        stmt.first::<WatchChannel>(None).await.map_err(backend)
    }

    async fn list_by_calendar_id(&self, calendar_id: &str) -> Result<Vec<WatchChannel>, RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_all(&self) -> Result<Vec<WatchChannel>, RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_LIST_ALL_SQL)
            .bind(&[])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_unexpired_by_calendar_id(
        &self,
        calendar_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<WatchChannel>, RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id), D1Type::Text(now_rfc3339)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn delete_by_id(&self, id: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_DELETE_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete_by_calendar_id(&self, calendar_id: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

/// Row projection for `SELECT google_event_id …` inflight listing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct OperationGoogleEventIdRow {
    google_event_id: String,
}

#[async_trait::async_trait(?Send)]
impl CalendarEventOperationRepo for D1CalendarEventOperationRepo {
    async fn insert(
        &self,
        op: NewCalendarEventOperation,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let stmt = self
            .db
            .prepare(OPERATION_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&op.user_id),
                D1Type::Text(&op.calendar_id),
                D1Type::Text(&op.local_event_id),
                D1Type::Text(&op.google_event_id),
                D1Type::Text(&op.verb),
                D1Type::Text(&op.payload_fingerprint),
                D1Type::Text(&op.payload_json),
                D1Type::Text(&op.status),
                D1Type::Text(&op.google_etag),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        Ok(id)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEventOperation>, RepoError> {
        let stmt = self
            .db
            .prepare(OPERATION_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<CalendarEventOperation>(None)
            .await
            .map_err(backend)
    }

    async fn list_by_statuses(
        &self,
        statuses: &[&str],
    ) -> Result<Vec<CalendarEventOperation>, RepoError> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let (sql, args) = build_operation_list_by_statuses_sql(statuses);
        let refs: Vec<D1Type> = args.iter().map(|arg| D1Type::Text(arg)).collect();
        let stmt = self.db.prepare(&sql).bind_refs(&refs).map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_inflight_google_ids(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<String>, RepoError> {
        let stmt = self
            .db
            .prepare(OPERATION_LIST_INFLIGHT_GOOGLE_IDS_SQL)
            .bind_refs(&[D1Type::Text(calendar_id)])
            .map_err(backend)?;
        let rows: Vec<OperationGoogleEventIdRow> = query_vec(stmt).await?;
        Ok(rows.into_iter().map(|row| row.google_event_id).collect())
    }

    async fn update_status(
        &self,
        id: &str,
        status: &str,
        last_error: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(OPERATION_UPDATE_STATUS_SQL)
            .bind_refs(&[
                D1Type::Text(status),
                D1Type::Text(last_error),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn update_progress(
        &self,
        id: &str,
        status: &str,
        google_event_id: &str,
        local_event_id: &str,
        google_etag: &str,
        last_error: &str,
        bump_attempt: bool,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let bump = if bump_attempt { "1" } else { "0" };
        let stmt = self
            .db
            .prepare(OPERATION_UPDATE_PROGRESS_SQL)
            .bind_refs(&[
                D1Type::Text(status),
                D1Type::Text(google_event_id),
                D1Type::Text(local_event_id),
                D1Type::Text(google_etag),
                D1Type::Text(last_error),
                D1Type::Text(bump),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}
