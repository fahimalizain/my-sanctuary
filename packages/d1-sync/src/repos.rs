//! rusqlite implementations of api-core calendar/token repo traits.
//!
//! Bind order mirrors `apps/worker/src/db/calendar.rs` and
//! `apps/worker/src/db/mod.rs` (`D1TokenRepo`). SQL constants come from
//! `api_core::repo::*`.

use api_core::models::{
    CalendarEvent, CalendarEventOperation, GoogleCalendar, GoogleOAuthToken, NewCalendar,
    NewCalendarEvent, NewCalendarEventOperation, NewToken, NewWatchChannel, WatchChannel,
};
use api_core::repo::{
    build_event_upsert_if_owner_sql, build_event_upsert_sql, CalendarEventOperationRepo,
    CalendarEventRepo, CalendarRepo, RepoError, TokenRepo, WatchChannelRepo,
    CALENDAR_BEGIN_REPLICA_RESEED_SQL, CALENDAR_BUMP_DIRTY_REQUESTED_SQL, CALENDAR_DELETE_SQL,
    CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL, CALENDAR_GET_BY_ID_SQL, CALENDAR_GET_BY_ID_UNFILTERED_SQL,
    CALENDAR_LEASE_HELD_SQL, CALENDAR_LIST_BY_USER_ID_SQL, CALENDAR_LIST_STATE_GET_SQL,
    CALENDAR_LIST_STATE_UPSERT_SQL, CALENDAR_LIST_SYNC_ENABLED_SQL, CALENDAR_LIST_USER_IDS_SQL,
    CALENDAR_MARK_DIRTY_APPLIED_SQL, CALENDAR_RECORD_SYNC_ATTEMPT_SQL,
    CALENDAR_RECORD_SYNC_FAILURE_SQL, CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL,
    CALENDAR_RECORD_SYNC_SUCCESS_SQL, CALENDAR_RELEASE_LEASE_SQL, CALENDAR_RENEW_LEASE_SQL,
    CALENDAR_SET_EVENT_LABELS_SQL, CALENDAR_SET_SYNC_ENABLED_SQL, CALENDAR_SET_WATCH_COVERAGE_SQL,
    CALENDAR_TRY_ACQUIRE_LEASE_SQL, CALENDAR_UPDATE_SYNC_STATE_SQL, CALENDAR_UPSERT_SQL,
    EVENT_DELETE_BY_GOOGLE_EVENT_ID_IF_OWNER_SQL, EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL,
    EVENT_DELETE_SQL, EVENT_DELETE_STALE_SQL, EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL,
    EVENT_GET_BY_ID_SQL, EVENT_GET_ID_BY_NATURAL_KEY_SQL, EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL,
    EVENT_LIST_RUNNING_BY_USER_ID_SQL, EVENT_UPSERT_CHUNK_SIZE, OPERATION_GET_BY_ID_SQL,
    OPERATION_INSERT_SQL, OPERATION_UPDATE_PROGRESS_SQL, OPERATION_UPDATE_STATUS_SQL,
    TOKEN_DELETE_SQL, TOKEN_GET_BY_USER_ID_SQL, TOKEN_UPSERT_SQL,
    WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL, WATCH_CHANNEL_DELETE_BY_ID_SQL,
    WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL, WATCH_CHANNEL_INSERT_SQL, WATCH_CHANNEL_LIST_ALL_SQL,
    WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL, WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL,
};
use rusqlite::Connection;
use serde::Deserialize;

use crate::{exec, exec_changes, query_one, query_vec, Db};

fn lock(db: &Db) -> Result<std::sync::MutexGuard<'_, Connection>, RepoError> {
    db.lock()
        .map_err(|_| RepoError::Backend("sqlite mutex poisoned".into()))
}

fn now_rfc3339_wall() -> String {
    // Repos that generate timestamps for upsert without a caller clock use a
    // fixed far-past stamp only if needed; calendar upsert in tests seeds rows
    // directly. Prefer a deterministic clock for pure SQL tests.
    "2026-01-01T00:00:00Z".to_string()
}

// ── CalendarRepo ────────────────────────────────────────────────────────────

pub struct SqliteCalendarRepo {
    db: Db,
}

impl SqliteCalendarRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarRepo for SqliteCalendarRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, CALENDAR_LIST_BY_USER_ID_SQL, &[&user_id])
    }

    async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, CALENDAR_LIST_SYNC_ENABLED_SQL, &[])
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, CALENDAR_GET_BY_ID_SQL, &[&id])
    }

    async fn get_by_id_unfiltered(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, CALENDAR_GET_BY_ID_UNFILTERED_SQL, &[&id])
    }

    async fn get_by_google_cal_id(
        &self,
        user_id: &str,
        google_cal_id: &str,
    ) -> Result<Option<GoogleCalendar>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(
            &conn,
            CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL,
            &[&user_id, &google_cal_id],
        )
    }

    async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError> {
        self.upsert_batch(vec![calendar]).await
    }

    async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        for calendar in calendars {
            let id = uuid::Uuid::new_v4().to_string();
            let now = now_rfc3339_wall();
            let is_primary = i64::from(calendar.is_primary);
            let sync_enabled = i64::from(calendar.sync_enabled);
            let last_synced: Option<&str> = calendar.last_synced_at.as_deref();
            exec(
                &conn,
                CALENDAR_UPSERT_SQL,
                &[
                    &id,
                    &calendar.user_id,
                    &calendar.google_calendar_id,
                    &calendar.summary,
                    &calendar.time_zone,
                    &is_primary,
                    &calendar.access_role,
                    &sync_enabled,
                    &calendar.sync_token,
                    &last_synced,
                    &now,
                    &now,
                ],
            )?;
        }
        Ok(())
    }

    async fn update_sync_state(
        &self,
        id: &str,
        sync_token: &str,
        last_synced_at_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        let now = now_rfc3339_wall();
        exec(
            &conn,
            CALENDAR_UPDATE_SYNC_STATE_SQL,
            &[&sync_token, &last_synced_at_rfc3339, &now, &id],
        )
    }

    async fn record_sync_attempt(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        // Binds: now, now, id
        exec(
            &conn,
            CALENDAR_RECORD_SYNC_ATTEMPT_SQL,
            &[&now_rfc3339, &now_rfc3339, &id],
        )
    }

    async fn record_sync_success(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        // Binds: token, last_synced_at, last_success_at, last_attempt_at,
        // fingerprint, updated_at, id
        exec(
            &conn,
            CALENDAR_RECORD_SYNC_SUCCESS_SQL,
            &[
                &sync_token,
                &now_rfc3339,
                &now_rfc3339,
                &now_rfc3339,
                &query_fingerprint,
                &now_rfc3339,
                &id,
            ],
        )
    }

    async fn record_sync_success_if_owner(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let conn = lock(&self.db)?;
        // Binds: token, last_synced_at, last_success_at, last_attempt_at,
        // fingerprint, updated_at, id, lease_owner, now (lease check).
        let changes = exec_changes(
            &conn,
            CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL,
            &[
                &sync_token,
                &now_rfc3339,
                &now_rfc3339,
                &now_rfc3339,
                &query_fingerprint,
                &now_rfc3339,
                &id,
                &lease_owner,
                &now_rfc3339,
            ],
        )?;
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
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_RECORD_SYNC_FAILURE_SQL,
            &[
                &error_code,
                &sync_status,
                &next_retry_rfc3339,
                &now_rfc3339,
                &id,
            ],
        )
    }

    async fn begin_replica_reseed(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_BEGIN_REPLICA_RESEED_SQL,
            &[&now_rfc3339, &id],
        )
    }

    async fn try_acquire_lease(
        &self,
        id: &str,
        owner: &str,
        now_rfc3339: &str,
        expires_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        // Binds: owner, expires, now, id, owner, now.
        {
            let conn = lock(&self.db)?;
            exec(
                &conn,
                CALENDAR_TRY_ACQUIRE_LEASE_SQL,
                &[
                    &owner,
                    &expires_rfc3339,
                    &now_rfc3339,
                    &id,
                    &owner,
                    &now_rfc3339,
                ],
            )?;
        }
        // Verify ownership (same as D1).
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
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_RELEASE_LEASE_SQL,
            &[&now_rfc3339, &id, &owner],
        )
    }

    async fn renew_lease(
        &self,
        id: &str,
        owner: &str,
        expires_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let conn = lock(&self.db)?;
        let changes = exec_changes(
            &conn,
            CALENDAR_RENEW_LEASE_SQL,
            &[&expires_rfc3339, &now_rfc3339, &id, &owner],
        )?;
        Ok(changes > 0)
    }

    async fn bump_dirty_requested(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_BUMP_DIRTY_REQUESTED_SQL,
            &[&now_rfc3339, &id],
        )
    }

    async fn mark_dirty_applied(
        &self,
        id: &str,
        generation: i64,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        // Binds: gen, now, id, gen
        exec(
            &conn,
            CALENDAR_MARK_DIRTY_APPLIED_SQL,
            &[&generation, &now_rfc3339, &id, &generation],
        )
    }

    async fn set_sync_enabled(
        &self,
        id: &str,
        enabled: bool,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        let enabled_i = i64::from(enabled);
        exec(
            &conn,
            CALENDAR_SET_SYNC_ENABLED_SQL,
            &[&enabled_i, &now_rfc3339, &id],
        )
    }

    async fn set_event_labels(
        &self,
        id: &str,
        event_labels_json: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_SET_EVENT_LABELS_SQL,
            &[&event_labels_json, &now_rfc3339, &now_rfc3339, &id],
        )
    }

    async fn set_watch_coverage(
        &self,
        id: &str,
        coverage: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_SET_WATCH_COVERAGE_SQL,
            &[&coverage, &now_rfc3339, &id],
        )
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_DELETE_SQL,
            &[&now_rfc3339, &now_rfc3339, &id],
        )
    }

    async fn get_calendar_list_sync_token(
        &self,
        user_id: &str,
    ) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct Row {
            sync_token: String,
        }
        let conn = lock(&self.db)?;
        let row: Option<Row> = query_one(&conn, CALENDAR_LIST_STATE_GET_SQL, &[&user_id])?;
        Ok(row.map(|r| r.sync_token))
    }

    async fn set_calendar_list_sync_token(
        &self,
        user_id: &str,
        token: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            CALENDAR_LIST_STATE_UPSERT_SQL,
            &[&user_id, &token, &now_rfc3339],
        )
    }

    async fn list_user_ids_with_calendars(&self) -> Result<Vec<String>, RepoError> {
        #[derive(Deserialize)]
        struct Row {
            user_id: String,
        }
        let conn = lock(&self.db)?;
        let rows: Vec<Row> = query_vec(&conn, CALENDAR_LIST_USER_IDS_SQL, &[])?;
        Ok(rows.into_iter().map(|r| r.user_id).collect())
    }
}

// ── CalendarEventRepo ───────────────────────────────────────────────────────

pub struct SqliteCalendarEventRepo {
    db: Db,
}

impl SqliteCalendarEventRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    fn lookup_id_by_natural_key(
        conn: &Connection,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<String>, RepoError> {
        #[derive(Deserialize)]
        struct EventIdRow {
            id: String,
        }
        let row: Option<EventIdRow> = query_one(
            conn,
            EVENT_GET_ID_BY_NATURAL_KEY_SQL,
            &[&calendar_id, &google_event_id],
        )?;
        Ok(row.map(|r| r.id))
    }

    fn run_upsert(conn: &Connection, sql: &str, args: &[String]) -> Result<(), RepoError> {
        let params: Vec<&dyn rusqlite::types::ToSql> = args
            .iter()
            .map(|a| a as &dyn rusqlite::types::ToSql)
            .collect();
        exec(conn, sql, &params)
    }

    fn run_upsert_changes(
        conn: &Connection,
        sql: &str,
        args: &[String],
    ) -> Result<usize, RepoError> {
        let params: Vec<&dyn rusqlite::types::ToSql> = args
            .iter()
            .map(|a| a as &dyn rusqlite::types::ToSql)
            .collect();
        exec_changes(conn, sql, &params)
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarEventRepo for SqliteCalendarEventRepo {
    async fn upsert(
        &self,
        event: NewCalendarEvent,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        let conn = lock(&self.db)?;
        let id = match Self::lookup_id_by_natural_key(
            &conn,
            &event.calendar_id,
            &event.google_event_id,
        )? {
            Some(existing) => existing,
            None => uuid::Uuid::new_v4().to_string(),
        };
        let (sql, args) = build_event_upsert_sql(&[event], now_rfc3339, vec![id.clone()]);
        Self::run_upsert(&conn, &sql, &args)?;
        Ok(id)
    }

    async fn upsert_batch(
        &self,
        events: Vec<NewCalendarEvent>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        for chunk in events.chunks(EVENT_UPSERT_CHUNK_SIZE) {
            let mut ids = Vec::with_capacity(chunk.len());
            for event in chunk {
                let id = match Self::lookup_id_by_natural_key(
                    &conn,
                    &event.calendar_id,
                    &event.google_event_id,
                )? {
                    Some(existing) => existing,
                    None => uuid::Uuid::new_v4().to_string(),
                };
                ids.push(id);
            }
            let (sql, args) = build_event_upsert_sql(chunk, now_rfc3339, ids);
            Self::run_upsert(&conn, &sql, &args)?;
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
        let conn = lock(&self.db)?;
        for chunk in events.chunks(EVENT_UPSERT_CHUNK_SIZE) {
            let calendar_id = chunk[0].calendar_id.as_str();
            let mut ids = Vec::with_capacity(chunk.len());
            for event in chunk {
                let id = match Self::lookup_id_by_natural_key(
                    &conn,
                    &event.calendar_id,
                    &event.google_event_id,
                )? {
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
            let changes = Self::run_upsert_changes(&conn, &sql, &args)?;
            if changes == 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEvent>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, EVENT_GET_BY_ID_SQL, &[&id])
    }

    async fn get_by_calendar_and_google_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<CalendarEvent>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(
            &conn,
            EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL,
            &[&calendar_id, &google_event_id],
        )
    }

    async fn list_by_user_id_and_time_range(
        &self,
        user_id: &str,
        start_rfc3339: &str,
        end_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL,
            &[&user_id, &end_rfc3339, &start_rfc3339],
        )
    }

    async fn list_running_by_user_id(
        &self,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            EVENT_LIST_RUNNING_BY_USER_ID_SQL,
            &[&user_id, &now_rfc3339, &now_rfc3339],
        )
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            EVENT_DELETE_SQL,
            &[&now_rfc3339, &now_rfc3339, &id],
        )
    }

    async fn delete_by_google_event_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL,
            &[&now_rfc3339, &now_rfc3339, &calendar_id, &google_event_id],
        )
    }

    async fn delete_by_google_event_id_if_owner(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            EVENT_DELETE_BY_GOOGLE_EVENT_ID_IF_OWNER_SQL,
            &[
                &now_rfc3339,
                &now_rfc3339,
                &calendar_id,
                &google_event_id,
                &lease_owner,
                &now_rfc3339,
            ],
        )?;
        #[derive(Deserialize)]
        struct LeaseHeldRow {
            #[allow(dead_code)]
            ok: i64,
        }
        let held: Option<LeaseHeldRow> = query_one(
            &conn,
            CALENDAR_LEASE_HELD_SQL,
            &[&calendar_id, &lease_owner, &now_rfc3339],
        )?;
        Ok(held.is_some())
    }

    async fn delete_stale(
        &self,
        calendar_id: &str,
        older_than_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            EVENT_DELETE_STALE_SQL,
            &[
                &now_rfc3339,
                &now_rfc3339,
                &calendar_id,
                &older_than_rfc3339,
            ],
        )
    }
}

// ── WatchChannelRepo ────────────────────────────────────────────────────────

pub struct SqliteWatchChannelRepo {
    db: Db,
}

impl SqliteWatchChannelRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl WatchChannelRepo for SqliteWatchChannelRepo {
    async fn insert(
        &self,
        channel: NewWatchChannel,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        let conn = lock(&self.db)?;
        let id = uuid::Uuid::new_v4().to_string();
        exec(
            &conn,
            WATCH_CHANNEL_INSERT_SQL,
            &[
                &id,
                &channel.calendar_id,
                &channel.channel_id,
                &channel.resource_id,
                &channel.token,
                &channel.expiration,
                &now_rfc3339,
                &now_rfc3339,
            ],
        )?;
        Ok(id)
    }

    async fn get_by_channel_id(
        &self,
        channel_id: &str,
    ) -> Result<Option<WatchChannel>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL, &[&channel_id])
    }

    async fn list_by_calendar_id(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<WatchChannel>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL,
            &[&calendar_id],
        )
    }

    async fn list_all(&self) -> Result<Vec<WatchChannel>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, WATCH_CHANNEL_LIST_ALL_SQL, &[])
    }

    async fn list_unexpired_by_calendar_id(
        &self,
        calendar_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<WatchChannel>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL,
            &[&calendar_id, &now_rfc3339],
        )
    }

    async fn delete_by_id(&self, id: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(&conn, WATCH_CHANNEL_DELETE_BY_ID_SQL, &[&id])
    }

    async fn delete_by_calendar_id(&self, calendar_id: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL,
            &[&calendar_id],
        )
    }
}

// ── CalendarEventOperationRepo ──────────────────────────────────────────────

pub struct SqliteCalendarEventOperationRepo {
    db: Db,
}

impl SqliteCalendarEventOperationRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarEventOperationRepo for SqliteCalendarEventOperationRepo {
    async fn insert(
        &self,
        op: NewCalendarEventOperation,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        let conn = lock(&self.db)?;
        let id = uuid::Uuid::new_v4().to_string();
        exec(
            &conn,
            OPERATION_INSERT_SQL,
            &[
                &id,
                &op.user_id,
                &op.calendar_id,
                &op.local_event_id,
                &op.google_event_id,
                &op.verb,
                &op.payload_fingerprint,
                &op.payload_json,
                &op.status,
                &op.google_etag,
                &now_rfc3339,
                &now_rfc3339,
            ],
        )?;
        Ok(id)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEventOperation>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, OPERATION_GET_BY_ID_SQL, &[&id])
    }

    async fn list_by_statuses(
        &self,
        _statuses: &[&str],
    ) -> Result<Vec<CalendarEventOperation>, RepoError> {
        // Cron repair may call this; empty is correct when nothing is inflight.
        Ok(Vec::new())
    }

    async fn list_inflight_google_ids(
        &self,
        _calendar_id: &str,
    ) -> Result<Vec<String>, RepoError> {
        // Cron / replica skip path; empty means no inflight writes.
        Ok(Vec::new())
    }

    async fn update_status(
        &self,
        id: &str,
        status: &str,
        last_error: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            OPERATION_UPDATE_STATUS_SQL,
            &[&status, &last_error, &now_rfc3339, &id],
        )
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
        let conn = lock(&self.db)?;
        let bump = if bump_attempt { "1" } else { "0" };
        exec(
            &conn,
            OPERATION_UPDATE_PROGRESS_SQL,
            &[
                &status,
                &google_event_id,
                &local_event_id,
                &google_etag,
                &last_error,
                &bump,
                &now_rfc3339,
                &id,
            ],
        )
    }
}

// ── TokenRepo ───────────────────────────────────────────────────────────────

pub struct SqliteTokenRepo {
    db: Db,
}

impl SqliteTokenRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl TokenRepo for SqliteTokenRepo {
    async fn get_by_user_id(
        &self,
        user_id: &str,
    ) -> Result<Option<GoogleOAuthToken>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, TOKEN_GET_BY_USER_ID_SQL, &[&user_id])
    }

    async fn upsert(&self, token: NewToken) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339_wall();
        let refresh: Option<&str> = token.refresh_token.as_deref();
        let scope: Option<&str> = token.scope.as_deref();
        exec(
            &conn,
            TOKEN_UPSERT_SQL,
            &[
                &id,
                &token.user_id,
                &token.access_token,
                &refresh,
                &token.expiry,
                &token.token_type,
                &scope,
                &now,
                &now,
            ],
        )
    }

    async fn delete(&self, user_id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            TOKEN_DELETE_SQL,
            &[&now_rfc3339, &now_rfc3339, &user_id],
        )
    }
}
